use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{anyhow, bail, Context, Result};
pub use inputs::{AccountInput, AuthSchemeInput, GenesisInput};
use miden_lib::{
    accounts::{faucets::create_basic_fungible_faucet, wallets::create_basic_wallet},
    AuthScheme,
};
use miden_node_store::genesis::GenesisState;
use miden_node_utils::config::load_config;
use miden_objects::{
    accounts::{Account, AccountData, AccountStorageMode, AccountType, AuthSecretKey},
    assets::TokenSymbol,
    crypto::{
        dsa::rpo_falcon512::SecretKey,
        rand::RpoRandomCoin,
        utils::{hex_to_bytes, Serializable},
    },
    Digest, Felt, ONE,
};
use tracing::info;

mod inputs;

const DEFAULT_ACCOUNTS_DIR: &str = "accounts/";

// MAKE GENESIS
// ================================================================================================

/// Generates a genesis file and associated account files based on a specified genesis input
///
/// # Arguments
///
/// * `output_path` - A `PathBuf` reference to the path where the genesis file will be created.
/// * `force` - A boolean flag to determine if an existing genesis file should be overwritten.
/// * `inputs_path` - A `PathBuf` reference to the genesis inputs file's path.
///
/// # Returns
///
/// This function returns a `Result` type. On successful creation of the genesis file, it returns
/// `Ok(())`. If it fails at any point, due to issues like file existence checks or read/write
/// operations, it returns an `Err` with a detailed error message.
pub fn make_genesis(inputs_path: &PathBuf, output_path: &PathBuf, force: &bool) -> Result<()> {
    let inputs_path = Path::new(inputs_path);
    let output_path = Path::new(output_path);

    if !force {
        if let Ok(file_exists) = output_path.try_exists() {
            if file_exists {
                return Err(anyhow!("Failed to generate new genesis file {} because it already exists. Use the --force flag to overwrite.", output_path.display()));
            }
        } else {
            return Err(anyhow!("Failed to open {} file.", output_path.display()));
        }
    }

    if let Ok(file_exists) = inputs_path.try_exists() {
        if !file_exists {
            return Err(anyhow!(
                "The {} file does not exist. It is necessary to generate the genesis file. Use the --inputs-path flag to pass in the genesis input file.",
                inputs_path.display()
            ));
        }
    } else {
        return Err(anyhow!("Failed to open {} file.", inputs_path.display()));
    }

    let parent_path = match output_path.parent() {
        Some(path) => path,
        None => {
            return Err(anyhow!(
                "There has been an error processing output_path: {}",
                output_path.display()
            ))
        },
    };

    let genesis_input: GenesisInput = load_config(inputs_path).map_err(|err| {
        anyhow!("Failed to load {} genesis input file: {err}", inputs_path.display())
    })?;
    info!("Genesis input file: {} has successfully been loaded.", output_path.display());

    let accounts =
        create_accounts(&genesis_input.accounts.unwrap_or_default(), parent_path, force)?;
    info!(
        "Accounts have successfully been created at: {}/{}",
        parent_path.display(),
        DEFAULT_ACCOUNTS_DIR
    );

    let genesis_state = GenesisState::new(accounts, genesis_input.version, genesis_input.timestamp);
    fs::write(output_path, genesis_state.to_bytes()).unwrap_or_else(|_| {
        panic!("Failed to write genesis state to output file {}", output_path.display())
    });
    info!("Miden node genesis successful: {} has been created", output_path.display());

    Ok(())
}

/// Converts the provided list of account inputs into [Account] objects.
///
/// This function also writes the account data files into the default accounts directory.
fn create_accounts(
    accounts: &[AccountInput],
    parent_path: &Path,
    force: &bool,
) -> Result<Vec<Account>> {
    let mut accounts_path = PathBuf::from(&parent_path);
    accounts_path.push(DEFAULT_ACCOUNTS_DIR);

    if accounts_path.try_exists()? {
        if !force {
            bail!(
                "Failed to create accounts directory because it already exists. \
                Use the --force flag to overwrite."
            );
        }
        fs::remove_dir_all(&accounts_path).context("Failed to remove accounts directory")?;
    }

    fs::create_dir_all(&accounts_path).context("Failed to create accounts directory")?;

    let mut final_accounts = Vec::new();

    for account in accounts {
        // build offchain account data from account inputs
        let mut account_data = match account {
            AccountInput::BasicWallet(inputs) => {
                info!("Creating basic wallet account...");
                let init_seed = hex_to_bytes(&inputs.init_seed)?;

                let (auth_scheme, auth_secret_key) =
                    parse_auth_inputs(inputs.auth_scheme, &inputs.auth_seed)?;

                let storage_mode = parse_storage_mode(&inputs.storage_mode)?;

                let (account, account_seed) = create_basic_wallet(
                    init_seed,
                    auth_scheme,
                    AccountType::RegularAccountImmutableCode,
                    storage_mode,
                )?;

                AccountData::new(account, Some(account_seed), auth_secret_key)
            },
            AccountInput::BasicFungibleFaucet(inputs) => {
                info!("Creating fungible faucet account...");
                let init_seed = hex_to_bytes(&inputs.init_seed)?;

                let (auth_scheme, auth_secret_key) =
                    parse_auth_inputs(inputs.auth_scheme, &inputs.auth_seed)?;

                let storage_mode = parse_storage_mode(&inputs.storage_mode)?;

                let (account, account_seed) = create_basic_fungible_faucet(
                    init_seed,
                    TokenSymbol::try_from(inputs.token_symbol.as_str())?,
                    inputs.decimals,
                    Felt::try_from(inputs.max_supply)
                        .expect("max supply value is greater than or equal to the field modulus"),
                    storage_mode,
                    auth_scheme,
                )?;

                AccountData::new(account, Some(account_seed), auth_secret_key)
            },
        };

        // write account data to file
        let path = format!("{}/account{}.mac", accounts_path.display(), final_accounts.len());
        let path = Path::new(&path);

        if let Ok(path_exists) = path.try_exists() {
            if path_exists && !force {
                bail!("Failed to generate account file {} because it already exists. Use the --force flag to overwrite.", path.display());
            }
        }

        account_data.account.set_nonce(ONE)?;

        account_data.write(path)?;

        final_accounts.push(account_data.account);
    }

    Ok(final_accounts)
}

fn parse_auth_inputs(
    auth_scheme_input: AuthSchemeInput,
    auth_seed: &str,
) -> Result<(AuthScheme, AuthSecretKey)> {
    match auth_scheme_input {
        AuthSchemeInput::RpoFalcon512 => {
            let auth_seed: [u8; 32] = hex_to_bytes(auth_seed)?;
            let rng_seed = Digest::try_from(&auth_seed)?.into();
            let mut rng = RpoRandomCoin::new(rng_seed);
            let secret = SecretKey::with_rng(&mut rng);

            let auth_scheme = AuthScheme::RpoFalcon512 { pub_key: secret.public_key() };
            let auth_secret_key = AuthSecretKey::RpoFalcon512(secret);

            Ok((auth_scheme, auth_secret_key))
        },
    }
}

fn parse_storage_mode(storage_mode: &str) -> Result<AccountStorageMode> {
    match storage_mode.to_lowercase().as_str() {
        "on-chain" => Ok(AccountStorageMode::Public),
        "off-chain" => Ok(AccountStorageMode::Private),
        mode => Err(anyhow!("The provided value for storage_type ({mode}) does not match the expected values (on-chain, off-chain)"))
    }
}

// TESTS
// ================================================================================================

#[cfg(test)]
mod tests {
    use std::{fs, path::PathBuf};

    use figment::Jail;
    use miden_node_store::genesis::GenesisState;
    use miden_objects::{accounts::AccountData, utils::serde::Deserializable};

    use super::make_genesis;
    use crate::DEFAULT_GENESIS_FILE_PATH;

    #[test]
    fn test_make_genesis() {
        let genesis_inputs_file_path = PathBuf::from("genesis.toml");

        // node genesis configuration
        Jail::expect_with(|jail| {
            jail.create_file(
                genesis_inputs_file_path.as_path(),
                r#"
                version = 1
                timestamp = 1672531200

                [[accounts]]
                type = "BasicWallet"
                init_seed = "0xa123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
                auth_scheme = "RpoFalcon512"
                auth_seed = "0xb123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
                storage_mode = "off-chain"

                [[accounts]]
                type = "BasicFungibleFaucet"
                init_seed = "0xc123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
                auth_scheme = "RpoFalcon512"
                auth_seed = "0xd123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
                token_symbol = "POL"
                decimals = 12
                max_supply = 1000000
                storage_mode = "on-chain"
            "#,
            )?;

            let genesis_dat_file_path = PathBuf::from(DEFAULT_GENESIS_FILE_PATH);

            //  run make_genesis to generate genesis.dat and accounts folder and files
            make_genesis(&genesis_inputs_file_path, &genesis_dat_file_path, &true).unwrap();

            let a0_file_path = PathBuf::from("accounts/account0.mac");
            let a1_file_path = PathBuf::from("accounts/account1.mac");

            // assert that the genesis.dat and account files exist
            assert!(genesis_dat_file_path.exists());
            assert!(a0_file_path.exists());
            assert!(a1_file_path.exists());

            // deserialize accounts and genesis_state
            let a0 = AccountData::read(a0_file_path).unwrap();
            let a1 = AccountData::read(a1_file_path).unwrap();

            // assert that the accounts have the corresponding storage mode
            assert!(!a0.account.is_public());
            assert!(a1.account.is_public());

            let genesis_file_contents = fs::read(genesis_dat_file_path).unwrap();
            let genesis_state = GenesisState::read_from_bytes(&genesis_file_contents).unwrap();

            // build supposed genesis_state
            let supposed_genesis_state =
                GenesisState::new(vec![a0.account, a1.account], 1, 1672531200);

            // assert that both genesis_state(s) are eq
            assert_eq!(genesis_state, supposed_genesis_state);

            Ok(())
        });
    }
}
