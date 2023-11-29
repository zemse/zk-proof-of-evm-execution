use super::{helpers::derive_circuit_name, proof::Proof, real_verifier::RealVerifier};
use crate::error::Error;
use eth_types::keccak256;
use halo2_proofs::{
    halo2curves::bn256::{Bn256, Fr, G1Affine},
    plonk::{create_proof, keygen_pk, keygen_vk, Circuit, ProvingKey, VerifyingKey},
    poly::{
        commitment::ParamsProver,
        kzg::{
            commitment::{KZGCommitmentScheme, ParamsKZG},
            multiopen::ProverSHPLONK,
        },
    },
    transcript::{Blake2bWrite, Challenge255, TranscriptWriterBuffer},
    SerdeFormat,
};
use rand_chacha::{rand_core::SeedableRng, ChaCha20Rng, ChaChaRng};
use std::{
    fs::{create_dir_all, File},
    path::{Path, PathBuf},
    str::FromStr,
};
use zkevm_circuits::{
    instance::public_data_convert, super_circuit::SuperCircuit, util::SubCircuit,
};

const SERDE_FORMAT: SerdeFormat = SerdeFormat::RawBytes;

#[derive(Clone)]
pub struct RealProver {
    circuit: SuperCircuit<Fr>,
    degree: u32,
    dir_path: PathBuf,
    rng: ChaCha20Rng,
    pub general_params: Option<ParamsKZG<Bn256>>,
    pub verifier_params: Option<ParamsKZG<Bn256>>,
    pub circuit_proving_key: Option<ProvingKey<G1Affine>>,
    pub circuit_verifying_key: Option<VerifyingKey<G1Affine>>,
}

impl RealProver {
    pub fn from(circuit: SuperCircuit<Fr>, k: u32, dir_path: Option<PathBuf>) -> Self {
        Self {
            circuit,
            degree: k,
            dir_path: dir_path.unwrap_or(PathBuf::from_str("./out").unwrap()),
            rng: ChaChaRng::seed_from_u64(2),
            general_params: None,
            verifier_params: None,
            circuit_proving_key: None,
            circuit_verifying_key: None,
        }
    }

    pub fn load(&mut self) -> Result<&Self, Error> {
        self.set_general_params(None)?;
        self.set_verifier_params(None)?;
        self.set_circuit_params(None, None)?;
        Ok(self)
    }

    pub fn prove(&mut self) -> Result<Proof, Error> {
        self.load()?;
        let public_data = public_data_convert(&self.circuit.evm_circuit.block.clone().unwrap());
        let instances = self.circuit.instance();
        let instances_refs_intermediate = instances.iter().map(|v| &v[..]).collect::<Vec<&[Fr]>>();
        let mut transcript = Blake2bWrite::<_, G1Affine, Challenge255<_>>::init(vec![]);
        create_proof::<
            KZGCommitmentScheme<Bn256>,
            ProverSHPLONK<'_, Bn256>,
            Challenge255<G1Affine>,
            ChaChaRng,
            Blake2bWrite<Vec<u8>, G1Affine, Challenge255<G1Affine>>,
            _,
        >(
            self.general_params.as_mut().unwrap(),
            self.circuit_proving_key.as_mut().unwrap(),
            &[self.circuit.clone()],
            &[&instances_refs_intermediate],
            self.rng.to_owned(),
            &mut transcript,
        )
        .unwrap();

        let circuit_name = derive_circuit_name(&self.circuit);
        let proof = transcript.finalize();
        Ok(Proof::from(
            self.degree,
            proof,
            instances,
            circuit_name,
            self.circuit.params(),
            public_data,
            None,
        ))
    }

    pub fn verifier(&self) -> RealVerifier {
        RealVerifier {
            general_params: self
                .general_params
                .clone()
                .ok_or("params not available, please execute prover.load() first")
                .unwrap(),
            verifier_params: self.verifier_params.clone().unwrap(),
            circuit_verifying_key: self.circuit_verifying_key.clone().unwrap(),
        }
    }

    pub fn degree(mut self, k: u32) -> Self {
        self.degree = k;
        self
    }

    fn set_general_params(
        &mut self,
        params_override: Option<ParamsKZG<Bn256>>,
    ) -> Result<(), Error> {
        if params_override.is_some() {
            self.general_params = params_override;
            return Ok(());
        }

        if self.general_params.is_some() {
            return Ok(());
        }

        self.ensure_dir_exists();

        let path = self
            .dir_path
            .join(Path::new(&format!("kzg_general_params_{}", self.degree)));
        match File::open(path.clone()) {
            Ok(mut file) => {
                self.general_params =
                    Some(ParamsKZG::<Bn256>::read_custom(&mut file, SERDE_FORMAT)?);
            }
            Err(_) => {
                let general_params = ParamsKZG::<Bn256>::setup(self.degree, self.rng.clone());
                let mut file = File::create(path)?;
                general_params.write_custom(&mut file, SERDE_FORMAT)?;
                self.general_params = Some(general_params);
            }
        };
        Ok(())
    }

    fn set_verifier_params(
        &mut self,
        params_override: Option<ParamsKZG<Bn256>>,
    ) -> Result<(), Error> {
        if params_override.is_some() {
            self.verifier_params = params_override;
            return Ok(());
        }

        if self.verifier_params.is_some() {
            return Ok(());
        }

        self.ensure_dir_exists();

        let path = self
            .dir_path
            .join(Path::new(&format!("kzg_verifier_params_{}", self.degree)));
        match File::open(path.clone()) {
            Ok(mut file) => {
                self.verifier_params =
                    Some(ParamsKZG::<Bn256>::read_custom(&mut file, SERDE_FORMAT)?);
            }
            Err(_) => {
                let general_params = self.general_params.clone().unwrap();
                let verifier_params = general_params.verifier_params().to_owned();
                let mut file = File::create(path)?;
                verifier_params.write_custom(&mut file, SERDE_FORMAT)?;
                self.verifier_params = Some(verifier_params);
            }
        };
        Ok(())
    }

    pub fn set_circuit_params(
        &mut self,
        circuit_proving_key_override: Option<ProvingKey<G1Affine>>,
        circuit_verifying_key_override: Option<VerifyingKey<G1Affine>>,
    ) -> Result<(), Error> {
        if self.circuit_proving_key.is_some() && self.circuit_verifying_key.is_some() {
            return Ok(());
        }

        if circuit_proving_key_override.is_some() && circuit_verifying_key_override.is_some() {
            self.circuit_proving_key = circuit_proving_key_override;
            self.circuit_verifying_key = circuit_verifying_key_override;
            return Ok(());
        }

        let verifying_key_path = self.dir_path.join(Path::new(&format!(
            "{}_verifying_key_{}",
            derive_circuit_name(&self.circuit),
            self.degree
        )));

        if verifying_key_path.exists() && let Ok(mut file) = File::open(verifying_key_path.clone()) {
            self.circuit_verifying_key = Some(
                VerifyingKey::<G1Affine>::read::<File, SuperCircuit<Fr>>(
                    &mut file,
                    SERDE_FORMAT,
                    self.circuit.params(),
                ).unwrap(),
            );
        } else {
            let vk = keygen_vk(self.general_params.as_mut().unwrap(), &self.circuit).expect("keygen_vk should not fail");
            let mut file = File::create(verifying_key_path)?;
            vk.write(&mut file, SERDE_FORMAT)?;
            println!(
                "circuit_verifying_key hash {:?}",
                keccak256(format!("{:?}", vk).as_bytes())
            );
            self.circuit_verifying_key = Some(vk);
        }

        self.ensure_dir_exists();

        let proving_key_path = self.dir_path.join(Path::new(&format!(
            "{}_proving_key_{}",
            derive_circuit_name(&self.circuit),
            self.degree
        )));
        // TODO make PK gen code similar to VK
        match File::open(proving_key_path.clone()) {
            Ok(mut file) => {
                self.circuit_proving_key = Some(
                    ProvingKey::<G1Affine>::read::<File, SuperCircuit<Fr>>(
                        &mut file,
                        SERDE_FORMAT,
                        self.circuit.params(),
                    )
                    .unwrap(),
                );
            }
            Err(_) => {
                let pk = keygen_pk(
                    self.general_params.as_mut().unwrap(),
                    self.circuit_verifying_key.clone().unwrap(),
                    &self.circuit,
                )
                .expect("keygen_pk should not fail");
                println!(
                    "circuit_proving_key hash {:?}",
                    keccak256(format!("{:?}", pk).as_bytes())
                );
                // Skip writing proving key to file because it takes lot of time
                // TODO put this under a flag
                let mut file = File::create(proving_key_path)?;
                pk.write(&mut file, SERDE_FORMAT)?;
                self.circuit_proving_key = Some(pk);
            }
        };
        Ok(())
    }

    fn ensure_dir_exists(&self) {
        create_dir_all(self.dir_path.clone()).unwrap();
    }
}