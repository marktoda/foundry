use crate::abi::HEVMCalls;
use bytes::{BufMut, Bytes, BytesMut};
use ethers::{
    abi::{AbiEncode, Address},
    prelude::{k256::ecdsa::SigningKey, LocalWallet, Signer},
    types::{NameOrAddress, H256, U256},
    utils,
};
use revm::{CreateInputs, Database, EVMData};
use std::str::FromStr;

use super::Cheatcodes;

fn addr(private_key: U256) -> Result<Bytes, Bytes> {
    if private_key.is_zero() {
        return Err("Private key cannot be 0.".to_string().encode().into())
    }

    let mut bytes: [u8; 32] = [0; 32];
    private_key.to_big_endian(&mut bytes);

    let key = SigningKey::from_bytes(&bytes).map_err(|err| err.to_string().encode())?;
    let addr = utils::secret_key_to_address(&key);
    Ok(addr.encode().into())
}

fn sign(private_key: U256, digest: H256, chain_id: U256) -> Result<Bytes, Bytes> {
    if private_key.is_zero() {
        return Err("Private key cannot be 0.".to_string().encode().into())
    }

    let mut bytes: [u8; 32] = [0; 32];
    private_key.to_big_endian(&mut bytes);

    let key = SigningKey::from_bytes(&bytes).map_err(|err| err.to_string().encode())?;
    let wallet = LocalWallet::from(key).with_chain_id(chain_id.as_u64());

    // The `ecrecover` precompile does not use EIP-155
    let sig = wallet.sign_hash(digest);
    let recovered = sig.recover(digest).map_err(|err| err.to_string().encode())?;

    assert_eq!(recovered, wallet.address());

    let mut r_bytes = [0u8; 32];
    let mut s_bytes = [0u8; 32];
    sig.r.to_big_endian(&mut r_bytes);
    sig.s.to_big_endian(&mut s_bytes);

    Ok((sig.v, r_bytes, s_bytes).encode().into())
}

pub fn apply<DB: Database>(
    state: &mut Cheatcodes,
    data: &mut EVMData<'_, DB>,
    call: &HEVMCalls,
) -> Option<Result<Bytes, Bytes>> {
    Some(match call {
        HEVMCalls::Addr(inner) => addr(inner.0),
        HEVMCalls::Sign(inner) => sign(inner.0, inner.1.into(), data.env.cfg.chain_id),
        HEVMCalls::Label(inner) => {
            state.labels.insert(inner.0, inner.1.clone());
            Ok(Bytes::new())
        }
        _ => return None,
    })
}

pub fn process_create<DB: Database>(
    broadcast_sender: Address,
    bytecode: Bytes,
    data: &mut EVMData<'_, DB>,
    call: &mut CreateInputs,
) -> (Bytes, Option<NameOrAddress>, u64) {
    match call.scheme {
        revm::CreateScheme::Create => {
            call.caller = broadcast_sender;

            (bytecode, None, data.subroutine.account(broadcast_sender).info.nonce)
        }
        revm::CreateScheme::Create2 { salt } => {
            // Sanity checks for our CREATE2 deployer
            let create2_deployer =
                Address::from_str("0x4e59b44847b379578588920ca78fbf26c0b4956c").unwrap();

            data.subroutine.load_account(create2_deployer, data.db);

            let info = &data.subroutine.account(create2_deployer).info;
            match &info.code {
                Some(code) => {
                    if code.is_empty() {
                        panic!("CREATE2 Deployer not present on this chain. [0x4e59b44847b379578588920ca78fbf26c0b4956c]")
                    }
                }
                None => {
                    // SharedBacked
                    if data.db.code_by_hash(info.code_hash).is_empty() {
                        panic!("CREATE2 Deployer not present on this chain. [0x4e59b44847b379578588920ca78fbf26c0b4956c]")
                    }
                }
            }

            call.caller = create2_deployer;

            // We have to increment the nonce of the user address, since this create2 will be done
            // by the create2_deployer
            let account = data.subroutine.state().get_mut(&broadcast_sender).unwrap();
            let nonce = account.info.nonce;
            account.info.nonce += 1;

            // Proxy deployer requires the data to be on the following format `salt.init_code`
            let mut calldata = BytesMut::with_capacity(32 + bytecode.len());
            let mut salt_bytes = [0u8; 32];
            salt.to_big_endian(&mut salt_bytes);
            calldata.put_slice(&salt_bytes);
            calldata.put(bytecode);

            (calldata.freeze(), Some(NameOrAddress::Address(create2_deployer)), nonce)
        }
    }
}
