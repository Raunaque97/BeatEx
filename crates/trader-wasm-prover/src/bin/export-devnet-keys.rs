use {
    beatex_babyjubjub_groth16_spike::{
        deposit::DepositProver, release::CollateralReleaseProver, rollover::RolloverProver,
        transition::OrderProver,
    },
    serde::Serialize,
    sha2::{Digest, Sha256},
    std::{
        env,
        error::Error,
        ffi::OsString,
        fs::{self, OpenOptions},
        io::Write,
        path::{Path, PathBuf},
    },
};

const MANIFEST_FILE: &str = "manifest.json";
const RUNTIME_FILE: &str = "beatex_trader_wasm_prover_bg.wasm";

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Manifest {
    schema_version: u16,
    backend: &'static str,
    proving_key_encoding: &'static str,
    runtime: AssetDescriptor,
    circuits: CircuitEntries,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AssetDescriptor {
    url: &'static str,
    byte_length: usize,
    sha256: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CircuitEntry {
    circuit_version: u16,
    proving_key: AssetDescriptor,
}

#[derive(Serialize)]
struct CircuitEntries {
    balance: CircuitEntry,
    order: CircuitEntry,
    release: CircuitEntry,
    settlement: CircuitEntry,
}

fn main() -> Result<(), Box<dyn Error>> {
    let (output_dir, runtime_path) = arguments()?;
    fs::create_dir_all(&output_dir)?;

    let specs = [
        "balance-v1.pk",
        "order-v1.pk",
        "release-v1.pk",
        "settlement-v1.pk",
    ];
    for file in specs {
        reject_existing(&output_dir.join(file))?;
    }
    reject_existing(&output_dir.join(RUNTIME_FILE))?;
    reject_existing(&output_dir.join(MANIFEST_FILE))?;

    let runtime = fs::read(runtime_path)?;
    write_new(&output_dir.join(RUNTIME_FILE), &runtime)?;

    let artifacts = [
        DepositProver::deterministic_devnet()?.to_uncompressed_bytes()?,
        OrderProver::deterministic_devnet()?.to_uncompressed_bytes()?,
        CollateralReleaseProver::deterministic_devnet()?.to_uncompressed_bytes()?,
        RolloverProver::deterministic_devnet()?.to_uncompressed_bytes()?,
    ];
    let mut descriptors = Vec::with_capacity(specs.len());
    for (file, bytes) in specs.into_iter().zip(artifacts) {
        write_new(&output_dir.join(file), &bytes)?;
        descriptors.push(AssetDescriptor {
            url: file,
            byte_length: bytes.len(),
            sha256: format!("{:x}", Sha256::digest(&bytes)),
        });
    }

    let [balance, order, release, settlement]: [AssetDescriptor; 4] = descriptors
        .try_into()
        .map_err(|_| "expected exactly four proving-key descriptors")?;

    let manifest = Manifest {
        schema_version: 1,
        backend: "beatex-arkworks-groth16-wasm-v1",
        proving_key_encoding: "arkworks-canonical-uncompressed-v1",
        runtime: AssetDescriptor {
            url: RUNTIME_FILE,
            byte_length: runtime.len(),
            sha256: format!("{:x}", Sha256::digest(&runtime)),
        },
        circuits: CircuitEntries {
            balance: CircuitEntry {
                circuit_version: 1,
                proving_key: balance,
            },
            order: CircuitEntry {
                circuit_version: 1,
                proving_key: order,
            },
            release: CircuitEntry {
                circuit_version: 1,
                proving_key: release,
            },
            settlement: CircuitEntry {
                circuit_version: 1,
                proving_key: settlement,
            },
        },
    };
    let mut encoded = serde_json::to_vec_pretty(&manifest)?;
    encoded.push(b'\n');
    write_new(&output_dir.join(MANIFEST_FILE), &encoded)?;
    println!("exported Devnet proving keys to {}", output_dir.display());
    Ok(())
}

fn arguments() -> Result<(PathBuf, PathBuf), Box<dyn Error>> {
    let mut args = env::args_os();
    let executable = args
        .next()
        .unwrap_or_else(|| OsString::from("export-devnet-keys"));
    let Some(output_dir) = args.next() else {
        return Err(format!(
            "usage: {} <output-directory> <wasm-runtime>",
            Path::new(&executable).display()
        )
        .into());
    };
    let Some(runtime_path) = args.next() else {
        return Err(format!(
            "usage: {} <output-directory> <wasm-runtime>",
            Path::new(&executable).display()
        )
        .into());
    };
    if args.next().is_some() {
        return Err("expected an output directory and WASM runtime path".into());
    }
    Ok((PathBuf::from(output_dir), PathBuf::from(runtime_path)))
}

fn reject_existing(path: &Path) -> Result<(), Box<dyn Error>> {
    if path.exists() {
        return Err(format!("refusing to overwrite {}", path.display()).into());
    }
    Ok(())
}

fn write_new(path: &Path, bytes: &[u8]) -> Result<(), Box<dyn Error>> {
    let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}
