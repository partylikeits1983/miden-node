use std::{
    collections::BTreeMap,
    net::SocketAddr,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Instant,
};

use metrics::SeedingMetrics;
use miden_air::HashFunction;
use miden_block_prover::LocalBlockProver;
use miden_lib::{
    account::{auth::RpoFalcon512, faucets::BasicFungibleFaucet, wallets::BasicWallet},
    note::create_p2id_note,
    utils::Serializable,
};
use miden_node_block_producer::store::StoreClient;
use miden_node_proto::{domain::batch::BatchInputs, generated::store::rpc_client::RpcClient};
use miden_node_store::{DataDirectory, GenesisState, Store};
use miden_node_utils::tracing::grpc::OtelInterceptor;
use miden_objects::{
    Digest, Felt, ONE,
    account::{Account, AccountBuilder, AccountId, AccountStorageMode, AccountType},
    asset::{Asset, FungibleAsset, TokenSymbol},
    batch::{BatchAccountUpdate, BatchId, ProvenBatch},
    block::{BlockHeader, BlockInputs, BlockNumber, ProposedBlock, ProvenBlock},
    crypto::{
        dsa::rpo_falcon512::{PublicKey, SecretKey},
        rand::RpoRandomCoin,
    },
    note::{Note, NoteHeader, NoteId, NoteInclusionProof},
    transaction::{
        InputNote, InputNotes, OrderedTransactionHeaders, OutputNote, ProvenTransaction,
        ProvenTransactionBuilder, TransactionHeader,
    },
    vm::ExecutionProof,
};
use rand::Rng;
use rayon::{
    iter::{IntoParallelIterator, ParallelIterator},
    prelude::ParallelSlice,
};
use tokio::{fs, io::AsyncWriteExt, net::TcpListener, task};
use tonic::{service::interceptor::InterceptedService, transport::Channel};
use winterfell::Proof;

mod metrics;

// CONSTANTS
// ================================================================================================

const BATCHES_PER_BLOCK: usize = 16;
const TRANSACTIONS_PER_BATCH: usize = 16;

pub const ACCOUNTS_FILENAME: &str = "accounts.txt";

// SEED STORE
// ================================================================================================

/// Seeds the store with a given number of accounts.
pub async fn seed_store(
    data_directory: PathBuf,
    num_accounts: usize,
    public_accounts_percentage: u8,
) {
    let start = Instant::now();

    // Recreate the data directory (it should be empty for store bootstrapping).
    //
    // Ignore the error since it will also error if it does not exist.
    let _ = fs_err::remove_dir_all(&data_directory);
    fs_err::create_dir_all(&data_directory).expect("created data directory");

    // generate the faucet account and the genesis state
    let faucet = create_faucet();
    let genesis_state = GenesisState::new(vec![faucet.clone()], 1, 1);
    Store::bootstrap(genesis_state.clone(), &data_directory).expect("store should bootstrap");

    // start the store
    let (_, store_addr) = start_store(data_directory.clone()).await;
    let store_client = StoreClient::new(store_addr);

    // start generating blocks
    let accounts_filepath = data_directory.join(ACCOUNTS_FILENAME);
    let data_directory =
        miden_node_store::DataDirectory::load(data_directory).expect("data directory should exist");
    let genesis_header = genesis_state.into_block().unwrap().into_inner();
    let metrics = generate_blocks(
        num_accounts,
        public_accounts_percentage,
        faucet,
        genesis_header,
        &store_client,
        data_directory,
        accounts_filepath,
    )
    .await;

    println!("Total time: {:.3} seconds", start.elapsed().as_secs_f64());
    println!("{metrics}");
}

/// Generates batches of transactions to be inserted into the store.
///
/// The first transaction in each batch sends assets from the faucet to 255 accounts.
/// The rest of the transactions consume the notes created by the faucet in the previous block.
async fn generate_blocks(
    num_accounts: usize,
    public_accounts_percentage: u8,
    mut faucet: Account,
    genesis_block: ProvenBlock,
    store_client: &StoreClient,
    data_directory: DataDirectory,
    accounts_filepath: PathBuf,
) -> SeedingMetrics {
    // Each block is composed of [`BATCHES_PER_BLOCK`] batches, and each batch is composed of
    // [`TRANSACTIONS_PER_BATCH`] txs. The first note of the block is always a send assets tx
    // from the faucet to (BATCHES_PER_BLOCK * TRANSACTIONS_PER_BATCH) - 1 accounts. The rest of
    // the notes are consume note txs from the (BATCHES_PER_BLOCK * TRANSACTIONS_PER_BATCH) - 1
    // accounts that were minted in the previous block.
    let mut metrics = SeedingMetrics::new(data_directory.database_path());

    let mut account_ids = vec![];
    let mut note_nullifiers = vec![];

    let mut consume_notes_txs = vec![];

    let consumes_per_block = TRANSACTIONS_PER_BATCH * BATCHES_PER_BLOCK - 1;
    #[allow(clippy::cast_sign_loss, clippy::cast_precision_loss)]
    let num_public_accounts = (consumes_per_block as f64
        * (f64::from(public_accounts_percentage) / 100.0))
        .round() as usize;
    let num_private_accounts = consumes_per_block - num_public_accounts;
    // +1 to account for the first block with the send assets tx only
    let total_blocks = (num_accounts / consumes_per_block) + 1;

    // share random coin seed and key pair for all accounts to avoid key generation overhead
    let coin_seed: [u64; 4] = rand::rng().random();
    let rng = Arc::new(Mutex::new(RpoRandomCoin::new(coin_seed.map(Felt::new))));
    let key_pair = {
        let mut rng = rng.lock().unwrap();
        SecretKey::with_rng(&mut *rng)
    };

    let mut prev_block = genesis_block.clone();
    let mut current_anchor_header = genesis_block.header().clone();

    for i in 0..total_blocks {
        let mut block_txs = Vec::with_capacity(BATCHES_PER_BLOCK * TRANSACTIONS_PER_BATCH);

        // create public accounts and notes that mint assets for these accounts
        let (pub_accounts, pub_notes) = create_accounts_and_notes(
            num_public_accounts,
            AccountStorageMode::Private,
            &key_pair,
            &rng,
            faucet.id(),
            i,
        );

        // create private accounts and notes that mint assets for these accounts
        let (priv_accounts, priv_notes) = create_accounts_and_notes(
            num_private_accounts,
            AccountStorageMode::Private,
            &key_pair,
            &rng,
            faucet.id(),
            i,
        );

        let notes = [pub_notes, priv_notes].concat();
        let accounts = [pub_accounts, priv_accounts].concat();
        account_ids.extend(accounts.iter().map(Account::id));
        note_nullifiers.extend(notes.iter().map(|n| n.nullifier().prefix()));

        // create the tx that creates the notes
        let emit_note_tx = create_emit_note_tx(prev_block.header(), &mut faucet, notes.clone());

        // collect all the txs
        block_txs.push(emit_note_tx);
        block_txs.extend(consume_notes_txs);

        // create the batches with [TRANSACTIONS_PER_BATCH] txs each
        let batches: Vec<ProvenBatch> = block_txs
            .par_chunks(TRANSACTIONS_PER_BATCH)
            .map(|txs| create_batch(txs, prev_block.header()))
            .collect();

        // create the block and send it to the store
        let block_inputs = get_block_inputs(store_client, &batches, &mut metrics).await;

        // update blocks
        prev_block = apply_block(batches, block_inputs, store_client, &mut metrics).await;
        if current_anchor_header.block_epoch() != prev_block.header().block_epoch() {
            current_anchor_header = prev_block.header().clone();
        }

        // create the consume notes txs to be used in the next block
        let batch_inputs =
            get_batch_inputs(store_client, prev_block.header(), &notes, &mut metrics).await;
        consume_notes_txs = create_consume_note_txs(
            prev_block.header(),
            accounts,
            notes,
            &batch_inputs.note_proofs,
        );

        // track store size every 50 blocks
        if i % 50 == 0 {
            metrics.record_store_size();
        }
    }

    // dump account ids to a file
    let mut file = fs::File::create(accounts_filepath).await.unwrap();
    for id in account_ids {
        file.write_all(format!("{id}\n").as_bytes()).await.unwrap();
    }

    metrics
}

/// Given a list of batches and block inputs, creates a `ProvenBlock` and sends it to the store.
/// Tracks the insertion time on the metrics.
///
/// Returns the the inserted block.
async fn apply_block(
    batches: Vec<ProvenBatch>,
    block_inputs: BlockInputs,
    store_client: &StoreClient,
    metrics: &mut SeedingMetrics,
) -> ProvenBlock {
    let proposed_block = ProposedBlock::new(block_inputs, batches).unwrap();
    let proven_block = LocalBlockProver::new(0)
        .prove_without_batch_verification(proposed_block)
        .unwrap();
    let block_size: usize = proven_block.to_bytes().len();

    let start = Instant::now();
    store_client.apply_block(&proven_block).await.unwrap();
    metrics.track_block_insertion(start.elapsed(), block_size);

    proven_block
}

// HELPER FUNCTIONS
// ================================================================================================

/// Creates `num_accounts` accounts, and for each one creates a note that mint assets.
///
/// Returns a tuple with:
/// - The list of new accounts
/// - The list of new notes
fn create_accounts_and_notes(
    num_accounts: usize,
    storage_mode: AccountStorageMode,
    key_pair: &SecretKey,
    rng: &Arc<Mutex<RpoRandomCoin>>,
    faucet_id: AccountId,
    block_num: usize,
) -> (Vec<Account>, Vec<Note>) {
    (0..num_accounts)
        .into_par_iter()
        .map(|account_index| {
            let account = create_account(
                key_pair.public_key(),
                ((block_num * num_accounts) + account_index) as u64,
                storage_mode,
            );
            let note = {
                let mut rng = rng.lock().unwrap();
                create_note(faucet_id, account.id(), &mut rng)
            };
            (account, note)
        })
        .collect()
}

/// Creates a public P2ID note containing 10 tokens of the fungible asset associated with the
/// specified `faucet_id` and sent to the specified target account.
fn create_note(faucet_id: AccountId, target_id: AccountId, rng: &mut RpoRandomCoin) -> Note {
    let asset = Asset::Fungible(FungibleAsset::new(faucet_id, 10).unwrap());
    create_p2id_note(
        faucet_id,
        target_id,
        vec![asset],
        miden_objects::note::NoteType::Public,
        Felt::default(),
        rng,
    )
    .expect("note creation failed")
}

/// Creates a new private account with a given public key and anchor block. Generates the seed from
/// the given index.
fn create_account(public_key: PublicKey, index: u64, storage_mode: AccountStorageMode) -> Account {
    let init_seed: Vec<_> = index.to_be_bytes().into_iter().chain([0u8; 24]).collect();
    let (new_account, _) = AccountBuilder::new(init_seed.try_into().unwrap())
        .account_type(AccountType::RegularAccountImmutableCode)
        .storage_mode(storage_mode)
        .with_component(RpoFalcon512::new(public_key))
        .with_component(BasicWallet)
        .build()
        .unwrap();
    new_account
}

/// Creates a new faucet account.
fn create_faucet() -> Account {
    let coin_seed: [u64; 4] = rand::rng().random();
    let mut rng = RpoRandomCoin::new(coin_seed.map(Felt::new));
    let key_pair = SecretKey::with_rng(&mut rng);
    let init_seed = [0_u8; 32];

    let token_symbol = TokenSymbol::new("TEST").unwrap();
    let (new_faucet, _seed) = AccountBuilder::new(init_seed)
        .account_type(AccountType::FungibleFaucet)
        .storage_mode(AccountStorageMode::Private)
        .with_component(RpoFalcon512::new(key_pair.public_key()))
        .with_component(BasicFungibleFaucet::new(token_symbol, 2, Felt::new(u64::MAX)).unwrap())
        .build()
        .unwrap();
    new_faucet
}

/// Creates a proven batch from a list of transactions and a reference block.
fn create_batch(txs: &[ProvenTransaction], block_ref: &BlockHeader) -> ProvenBatch {
    let account_updates = txs
        .iter()
        .map(|tx| (tx.account_id(), BatchAccountUpdate::from_transaction(tx)))
        .collect();
    let input_notes = txs.iter().flat_map(|tx| tx.input_notes().iter().cloned()).collect();
    let output_notes = txs.iter().flat_map(|tx| tx.output_notes().iter().cloned()).collect();
    ProvenBatch::new(
        BatchId::from_transactions(txs.iter()),
        block_ref.commitment(),
        block_ref.block_num(),
        account_updates,
        InputNotes::new(input_notes).unwrap(),
        output_notes,
        BlockNumber::from(u32::MAX),
        OrderedTransactionHeaders::new_unchecked(txs.iter().map(TransactionHeader::from).collect()),
    )
    .unwrap()
}

/// For each pair of account and note, creates a transaction that consumes the note.
fn create_consume_note_txs(
    block_ref: &BlockHeader,
    accounts: Vec<Account>,
    notes: Vec<Note>,
    note_proofs: &BTreeMap<NoteId, NoteInclusionProof>,
) -> Vec<ProvenTransaction> {
    accounts
        .into_iter()
        .zip(notes)
        .map(|(account, note)| {
            let inclusion_proof = note_proofs.get(&note.id()).unwrap();
            create_consume_note_tx(
                block_ref,
                account,
                InputNote::authenticated(note, inclusion_proof.clone()),
            )
        })
        .collect()
}

/// Creates a transaction that creates an account and consumes the given input note.
///
/// The account is updated with the assets from the input note, and the nonce is set to 1.
fn create_consume_note_tx(
    block_ref: &BlockHeader,
    mut account: Account,
    input_note: InputNote,
) -> ProvenTransaction {
    let init_hash = account.init_commitment();

    input_note.note().assets().iter().for_each(|asset| {
        account.vault_mut().add_asset(*asset).unwrap();
    });

    let (id, vault, sorage, code, _) = account.into_parts();
    let updated_account = Account::from_parts(id, vault, sorage, code, ONE);

    ProvenTransactionBuilder::new(
        updated_account.id(),
        init_hash,
        updated_account.commitment(),
        Digest::default(),
        block_ref.block_num(),
        block_ref.commitment(),
        u32::MAX.into(),
        ExecutionProof::new(Proof::new_dummy(), HashFunction::default()),
    )
    .add_input_notes(vec![input_note])
    .build()
    .unwrap()
}

/// Creates a transaction from the faucet that creates the given output notes.
/// Updates the faucet account to increase the issuance slot and it's nonce.
fn create_emit_note_tx(
    block_ref: &BlockHeader,
    faucet: &mut Account,
    output_notes: Vec<Note>,
) -> ProvenTransaction {
    let initial_account_hash = faucet.commitment();

    let slot = faucet.storage().get_item(2).unwrap();
    faucet
        .storage_mut()
        .set_item(0, [slot[0], slot[1], slot[2], slot[3] + Felt::new(10)])
        .unwrap();

    let (id, vault, sorage, code, nonce) = faucet.clone().into_parts();
    let updated_faucet = Account::from_parts(id, vault, sorage, code, nonce + ONE);

    ProvenTransactionBuilder::new(
        updated_faucet.id(),
        initial_account_hash,
        updated_faucet.commitment(),
        Digest::default(),
        block_ref.block_num(),
        block_ref.commitment(),
        u32::MAX.into(),
        ExecutionProof::new(Proof::new_dummy(), HashFunction::default()),
    )
    .add_output_notes(output_notes.into_iter().map(OutputNote::Full).collect::<Vec<OutputNote>>())
    .build()
    .unwrap()
}

/// Gets the batch inputs from the store and tracks the query time on the metrics.
async fn get_batch_inputs(
    store_client: &StoreClient,
    block_ref: &BlockHeader,
    notes: &[Note],
    metrics: &mut SeedingMetrics,
) -> BatchInputs {
    let start = Instant::now();
    // Mark every note as unauthenticated, so that the store returns the inclusion proofs for all of
    // them
    let batch_inputs = store_client
        .get_batch_inputs(
            vec![(block_ref.block_num(), block_ref.commitment())].into_iter(),
            notes.iter().map(Note::id),
        )
        .await
        .unwrap();
    metrics.add_get_batch_inputs(start.elapsed());
    batch_inputs
}

/// Gets the block inputs from the store and tracks the query time on the metrics.
async fn get_block_inputs(
    store_client: &StoreClient,
    batches: &[ProvenBatch],
    metrics: &mut SeedingMetrics,
) -> BlockInputs {
    let start = Instant::now();
    let inputs = store_client
        .get_block_inputs(
            batches.iter().flat_map(ProvenBatch::updated_accounts),
            batches.iter().flat_map(ProvenBatch::created_nullifiers),
            batches.iter().flat_map(|batch| {
                batch
                    .input_notes()
                    .into_iter()
                    .filter_map(|note| note.header().map(NoteHeader::id))
            }),
            batches.iter().map(ProvenBatch::reference_block_num),
        )
        .await
        .unwrap();
    let get_block_inputs_time = start.elapsed();
    metrics.add_get_block_inputs(get_block_inputs_time);
    inputs
}

/// Runs the store with the given data directory. Returns a tuple with:
/// - a gRPC client to access the store
/// - the address of the store
pub async fn start_store(
    data_directory: PathBuf,
) -> (RpcClient<InterceptedService<Channel, OtelInterceptor>>, SocketAddr) {
    let rpc_listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("Failed to bind store RPC gRPC endpoint");
    let block_producer_listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("Failed to bind store block-producer gRPC endpoint");
    let store_addr = rpc_listener.local_addr().expect("Failed to get store RPC address");
    let ntx_builder_listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("Failed to bind store ntx-builder gRPC endpoint");
    let store_block_producer_addr = block_producer_listener
        .local_addr()
        .expect("Failed to get store block-producer address");
    let dir = data_directory.clone();

    task::spawn(async move {
        Store {
            rpc_listener,
            ntx_builder_listener,
            block_producer_listener,
            data_directory: dir,
        }
        .serve()
        .await
        .expect("Failed to start serving store");
    });

    let channel = tonic::transport::Endpoint::try_from(format!("http://{store_addr}",))
        .unwrap()
        .connect()
        .await
        .expect("Failed to connect to store");

    (RpcClient::with_interceptor(channel, OtelInterceptor), store_block_producer_addr)
}
