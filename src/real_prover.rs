use halo2_proofs::{
    halo2curves::bn256::{Bn256, Fq, Fr, G1Affine},
    plonk::{
        create_proof, keygen_pk, keygen_vk, verify_proof, Circuit, Error, ProvingKey, VerifyingKey,
    },
    poly::{
        commitment::ParamsProver,
        kzg::{
            commitment::{KZGCommitmentScheme, ParamsKZG},
            multiopen::{ProverSHPLONK, VerifierSHPLONK},
            strategy::SingleStrategy,
        },
    },
    transcript::{
        Blake2bRead, Blake2bWrite, Challenge255, TranscriptReadBuffer, TranscriptWriterBuffer,
    },
    SerdeFormat,
};
use rand_chacha::{rand_core::SeedableRng, ChaCha20Rng, ChaChaRng};
use snark_verifier::{
    loader::evm::EvmLoader,
    pcs::kzg::{Gwc19, KzgAs, KzgDecidingKey},
    system::halo2::{compile, transcript::evm::EvmTranscript, Config},
    verifier::{self, SnarkVerifier},
};
use std::{
    fmt::Debug,
    fs::{create_dir_all, File},
    io::Write,
    path::{Path, PathBuf},
    rc::Rc,
    str::FromStr,
};
use zkevm_circuits::{super_circuit::SuperCircuit, util::SubCircuit};

use crate::utils::derive_circuit_name;

// use crate::{derive_circuit_name, derive_k, CircuitExt};

type PlonkVerifier = verifier::plonk::PlonkVerifier<KzgAs<Bn256, Gwc19>>;

const SERDE_FORMAT: SerdeFormat = SerdeFormat::RawBytes;

#[derive(Clone)]
pub struct RealProver<ConcreteCircuit: Circuit<Fr> + SubCircuit<Fr> + Clone + Debug> {
    circuit: ConcreteCircuit,
    degree: u32,
    dir_path: PathBuf,
    rng: ChaCha20Rng,
    pub general_params: Option<ParamsKZG<Bn256>>,
    pub verifier_params: Option<ParamsKZG<Bn256>>,
    pub circuit_proving_key: Option<ProvingKey<G1Affine>>,
    pub circuit_verifying_key: Option<VerifyingKey<G1Affine>>,
}

impl<ConcreteCircuit: Circuit<Fr> + Circuit<Fr> + SubCircuit<Fr> + Clone + Debug>
    RealProver<ConcreteCircuit>
{
    pub fn from(circuit: ConcreteCircuit, k: u32) -> Self {
        Self {
            circuit,
            degree: k,
            dir_path: PathBuf::from_str("./out").unwrap(),
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

    pub fn run(&mut self, write_to_file: bool) -> Result<(Vec<u8>, Vec<Vec<Fr>>), Error> {
        self.load()?;
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

        let proof = transcript.finalize();
        if write_to_file {
            let proof_path = self.dir_path.join(Path::new(&format!(
                "{}_proof",
                derive_circuit_name(&self.circuit)
            )));

            let mut file = File::create(proof_path)?;
            file.write_all(proof.as_slice())?;
        }
        Ok((proof, instances))
    }

    pub fn verifier(&self) -> RealVerifier {
        RealVerifier {
            circuit_name: derive_circuit_name(&self.circuit),
            dir_path: self.dir_path.clone(),
            num_instance: vec![self.circuit.instance().len()],
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
        match File::open(verifying_key_path.clone()) {
            Ok(mut file) => {
                self.circuit_verifying_key = Some(
                    VerifyingKey::<G1Affine>::read::<File, ConcreteCircuit>(
                        &mut file,
                        SERDE_FORMAT,
                        self.circuit.params(),
                    )
                    .unwrap(),
                );
            }
            Err(_) => {
                let vk = keygen_vk(self.general_params.as_mut().unwrap(), &self.circuit)
                    .expect("keygen_vk should not fail");
                let mut file = File::create(verifying_key_path)?;
                vk.write(&mut file, SERDE_FORMAT)?;
                self.circuit_verifying_key = Some(vk);
            }
        };

        self.ensure_dir_exists();

        let proving_key_path = self.dir_path.join(Path::new(&format!(
            "{}_proving_key_{}",
            derive_circuit_name(&self.circuit),
            self.degree
        )));
        match File::open(proving_key_path.clone()) {
            Ok(mut file) => {
                self.circuit_proving_key = Some(
                    ProvingKey::<G1Affine>::read::<File, ConcreteCircuit>(
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

pub struct RealVerifier {
    pub circuit_name: String,
    pub dir_path: PathBuf,
    pub num_instance: Vec<usize>,
    pub general_params: ParamsKZG<Bn256>,
    pub verifier_params: ParamsKZG<Bn256>,
    pub circuit_verifying_key: VerifyingKey<G1Affine>,
}

impl RealVerifier {
    pub fn new(
        circuit_name: String,
        k: usize,
        dir_path: PathBuf,
        num_instance: Vec<usize>,
    ) -> Self {
        let path = dir_path.join(Path::new(&format!("kzg_general_params_{}", k)));
        let mut file = File::open(path).unwrap();
        let general_params = ParamsKZG::<Bn256>::read_custom(&mut file, SERDE_FORMAT).unwrap();

        let path = dir_path.join(Path::new(&format!("kzg_verifier_params_{}", k)));
        let mut file = File::open(path).unwrap();
        let verifier_params = ParamsKZG::<Bn256>::read_custom(&mut file, SERDE_FORMAT).unwrap();

        let verifying_key_path =
            dir_path.join(Path::new(&format!("{}_verifying_key_{}", circuit_name, k)));
        let mut file = File::open(verifying_key_path).unwrap();
        let circuit = SuperCircuit::default();
        let circuit_verifying_key = VerifyingKey::<G1Affine>::read::<File, SuperCircuit<Fr>>(
            &mut file,
            SERDE_FORMAT,
            circuit.params(),
        )
        .unwrap();

        Self {
            circuit_name,
            dir_path,
            num_instance,
            general_params,
            verifier_params,
            circuit_verifying_key,
        }
    }

    pub fn run(&self, proof: Vec<u8>, instance: Vec<Vec<Fr>>) -> Result<(), Error> {
        let strategy = SingleStrategy::new(&self.general_params);
        let instance_refs_intermediate = instance.iter().map(|v| &v[..]).collect::<Vec<&[Fr]>>();
        let mut verifier_transcript = Blake2bRead::<_, G1Affine, Challenge255<_>>::init(&proof[..]);

        verify_proof::<
            KZGCommitmentScheme<Bn256>,
            VerifierSHPLONK<'_, Bn256>,
            Challenge255<G1Affine>,
            Blake2bRead<&[u8], G1Affine, Challenge255<G1Affine>>,
            SingleStrategy<'_, Bn256>,
        >(
            &self.verifier_params,
            &self.circuit_verifying_key,
            strategy,
            &[&instance_refs_intermediate],
            &mut verifier_transcript,
        )
    }

    pub fn generate_yul(&self, write_to_file: bool) -> Result<String, Error> {
        let protocol = compile(
            &self.verifier_params,
            &self.circuit_verifying_key,
            Config::kzg().with_num_instance(self.num_instance.clone()),
        );
        let vk: KzgDecidingKey<Bn256> = (
            self.verifier_params.get_g()[0],
            self.verifier_params.g2(),
            self.verifier_params.s_g2(),
        )
            .into();

        let loader = EvmLoader::new::<Fq, Fr>();
        let protocol = protocol.loaded(&loader);
        let mut transcript = EvmTranscript::<_, Rc<EvmLoader>, _, _>::new(&loader);

        let instances = transcript.load_instances(self.num_instance.clone());
        let proof = PlonkVerifier::read_proof(&vk, &protocol, &instances, &mut transcript).unwrap();
        PlonkVerifier::verify(&vk, &protocol, &instances, &proof).unwrap();

        let source = loader.solidity_code();
        if write_to_file {
            let proof_path = self
                .dir_path
                .join(Path::new(&format!("{}_verifier.yul", self.circuit_name)));

            let mut file = File::create(proof_path)?;
            file.write_all(source.as_bytes())?;
        }
        Ok(source)
    }
}