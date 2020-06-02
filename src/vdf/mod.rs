use ramp::Int;
use ramp_primes::Generator;
use ramp_primes::Verification;
use std::cmp::Ordering;
use std::error::Error;
use std::fmt;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::{thread, time};

pub mod util;

/// InvalidCapError is returned when a non-prime cap is received in the vdf_worker
#[derive(Debug)]
pub struct InvalidCapError;

impl fmt::Display for InvalidCapError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Invalid cap value encountered!")
    }
}

impl Error for InvalidCapError {
    fn description(&self) -> &str {
        "Invalid cap value encountered!"
    }
}

/// The end result of the VDF which we still need to prove
#[derive(Debug, Clone, Default)]
pub struct VDFResult {
    pub result: Int,
    pub iterations: usize,
}

/// Traits that make calculating differences between VDFResults easier
impl Ord for VDFResult {
    fn cmp(&self, other: &Self) -> Ordering {
        self.iterations.cmp(&other.iterations)
    }
}

impl PartialOrd for VDFResult {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl PartialEq for VDFResult {
    fn eq(&self, other: &Self) -> bool {
        self.result == other.result && self.iterations == other.iterations
    }
}

impl Eq for VDFResult {}

/// Proof of an already calculated VDF that gets passed around between peers
#[derive(Debug, Clone, Default)]
pub struct VDFProof {
    pub modulus: Int,
    pub base: Int,
    pub output: VDFResult,
    pub cap: Int,
    pub proof: Int,
}

impl PartialEq for VDFProof {
    fn eq(&self, other: &Self) -> bool {
        self.output == other.output
            && self.proof == other.proof
            && self.modulus == other.modulus
            && self.base == other.base
            && self.cap == other.cap
    }
}

impl VDFProof {
    /// Returns a VDFProof based on a VDFResult
    pub fn new(modulus: &Int, base: &Int, result: &VDFResult, cap: &Int) -> Self {
        let mut proof = Int::one();
        let mut r = Int::one();
        let mut b: Int;

        for _ in 0..result.iterations {
            b = 2 * &r / cap;
            r = (2 * &r) % cap;
            proof = proof.pow_mod(&Int::from(2), modulus) * base.pow_mod(&b, modulus);
            proof %= modulus;
        }

        debug!(
            "Proof generated, final state: r: {:?}, proof: {:?}",
            r, proof
        );

        VDFProof {
            modulus: modulus.clone(),
            base: base.clone(),
            output: result.clone(),
            cap: cap.clone(),
            proof,
        }
    }

    /// A public function that a receiver can use to verify the correctness of the VDFProof
    pub fn verify(&self) -> bool {
        // Check first that the result isn't larger than the RSA base
        if self.proof > self.modulus {
            return false;
        }
        let r = Int::from(self.output.iterations).pow_mod(&Int::from(2), &self.cap);
        self.output.result
            == (self.proof.pow_mod(&self.cap, &self.modulus) * self.base.pow_mod(&r, &self.modulus))
                % &self.modulus
    }

    pub fn validate(&self) -> bool {
        self.modulus.gcd(&self.base) == 1 && self.modulus.gcd(&self.cap) == 1
    }

    /// Helper function for calculating the difference in iterations between two VDFProofs
    pub fn abs_difference(&self, other: &VDFProof) -> usize {
        if self.output > other.output {
            self.output.iterations - other.output.iterations
        } else {
            other.output.iterations - self.output.iterations
        }
    }
}

/// VDF is an options struct for calculating VDFProofs
#[derive(Debug, Clone)]
pub struct VDF {
    pub modulus: Int,
    pub base: Int,
    pub upper_bound: usize,
    pub cap: Int,
}

impl VDF {
    /// VDF builder with default options. Can be chained with estimate_upper_bound
    pub fn new(modulus: Int, base: Int, upper_bound: usize) -> Self {
        Self {
            modulus,
            base,
            upper_bound,
            cap: Int::zero(),
        }
    }

    /// Add a precomputed cap to the VDF
    pub fn with_cap(mut self, cap: Int) -> Self {
        self.cap = cap;
        self
    }

    /// Estimates the maximum number of sequential calculations that can fit in the fiven ms_bound
    /// millisecond threshold.
    pub fn estimate_upper_bound(mut self, ms_bound: u64) -> Self {
        let cap: Int = Generator::new_prime(128);
        let (capper, receiver) = self.clone().run_vdf_worker();

        let sleep_time = time::Duration::from_millis(ms_bound);
        thread::sleep(sleep_time);
        capper.send(cap).unwrap();

        if let Ok(res) = receiver.recv() {
            if let Ok(proof) = res {
                self.upper_bound = proof.output.iterations;
            }
        }
        self
    }

    /// A worker that does the actual calculation in a VDF. Returns a VDFProof based on initial
    /// parameters in the VDF.
    pub fn run_vdf_worker(self) -> (Sender<Int>, Receiver<Result<VDFProof, InvalidCapError>>) {
        let (caller_sender, worker_receiver): (Sender<Int>, Receiver<Int>) = channel();
        let (worker_sender, caller_receiver) = channel();

        thread::spawn(move || {
            let mut result = self.base.clone();
            let mut iterations: usize = 0;
            loop {
                result = result.pow_mod(&Int::from(2), &self.modulus);
                iterations += 1;

                if iterations == self.upper_bound || iterations == usize::MAX {
                    // Upper bound reached, stops iteration and calculates the proof
                    info!("Upper bound of {:?} reached, generating proof.", iterations);

                    // Copy pregenerated cap
                    let mut self_cap: Int = self.cap.clone();

                    // Check if default, check for primality if else
                    if self_cap == 0 {
                        self_cap = Generator::new_safe_prime(128);
                    } else if !Verification::verify_safe_prime(self_cap.clone()) {
                        if worker_sender.send(Err(InvalidCapError)).is_err() {
                            error!("Predefined cap was not a prime! Check the implementation!");
                        }
                        break;
                    }

                    // Generate the VDF proof
                    let vdf_result = VDFResult { result, iterations };
                    let proof = VDFProof::new(&self.modulus, &self.base, &vdf_result, &self_cap);

                    // Send proof to caller
                    if worker_sender.send(Ok(proof)).is_err() {
                        error!("Failed to send the proof to caller!");
                    }
                    break;
                } else {
                    // Try receiving a cap from the other participant on each iteration
                    if let Ok(cap) = worker_receiver.try_recv() {
                        // Cap received
                        info!("Received the cap {:?}, generating proof.", cap);

                        // Check for primality
                        if Verification::verify_safe_prime(cap.clone()) {
                            // Generate the VDF proof
                            let vdf_result = VDFResult { result, iterations };
                            let proof = VDFProof::new(&self.modulus, &self.base, &vdf_result, &cap);

                            debug!("Proof generated! {:?}", proof);

                            // Send proof to caller
                            if worker_sender.send(Ok(proof)).is_err() {
                                error!("Failed to send the proof to caller!");
                            }
                        } else {
                            error!("Received cap was not a prime!");
                            // Received cap was not a prime, send error to caller
                            if worker_sender.send(Err(InvalidCapError)).is_err() {
                                error!("Error sending InvalidCapError to caller!");
                            }
                        }
                        break;
                    } else {
                        continue;
                    }
                }
            }
        });

        (caller_sender, caller_receiver)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use std::str::FromStr;

    const RSA_2048: &str = "2519590847565789349402718324004839857142928212620403202777713783604366202070759555626401852588078440691829064124951508218929855914917618450280848912007284499268739280728777673597141834727026189637501497182469116507761337985909570009733045974880842840179742910064245869181719511874612151517265463228221686998754918242243363725908514186546204357679842338718477444792073993423658482382428119816381501067481045166037730605620161967625613384414360383390441495263443219011465754445417842402092461651572335077870774981712577246796292638635637328991215483143816789988504044536402352738195137863656439121201039712282120720357";

    #[test]
    fn is_deterministic() {
        let modulus = Int::from_str("91").unwrap();
        let prime1 = Generator::new_safe_prime(128);
        let prime2 = Generator::new_safe_prime(128);
        let diffiehellman = prime1 * prime2;
        let root_hashed = util::hash(&diffiehellman.to_string(), &modulus);

        // Create two VDFs with same inputs to check if they end up in the same result
        let cap = Generator::new_safe_prime(128);
        let verifiers_vdf =
            VDF::new(modulus.clone(), root_hashed.clone(), 100).with_cap(cap.clone());
        let provers_vdf = VDF::new(modulus, root_hashed, 100).with_cap(cap);

        let (_, receiver) = verifiers_vdf.run_vdf_worker();
        let (_, receiver2) = provers_vdf.run_vdf_worker();

        if let Ok(res) = receiver.recv() {
            if let Ok(proof) = res {
                println!("{:?}", proof);
                assert!(proof.verify());

                let our_proof = proof;

                if let Ok(res2) = receiver2.recv() {
                    if let Ok(proof2) = res2 {
                        assert!(proof2.verify());
                        let their_proof = proof2;
                        assert_eq!(our_proof, their_proof);
                    }
                }
            }
        }
    }

    #[test]
    fn proof_generation_should_not_output_1() {
        let rsa_int: Int = Int::from_str(RSA_2048).unwrap();
        let test_base = Int::from_str("273975204758323482518647057650935910029033545998029740555442236630396252096456910679263590843517508134497658008476158127265573236243497472004339585881829574516389309280434908522535813874784132925778706216962484806893788484458866921522138079204310134658546314859280874834357982917353721208006721702257898821421485300922162212913879813354820758401899992407626681460926091914067738363972343377914312084771698845025136003678820078357252237830183129038642605158054307794910714600870478670992899152190065675937480994793580299676128729094577156848113371559032886218228855881073503523841233471830943401905740461433929135192").unwrap();
        let test_result = Int::from_str("403642510913427162346289814221267076355601453217789745314752711072245370139545017132729754050733978350640501433581222579549785921751858432800504504624146523068045818752999039584099531047946165070360558276303055979412042063125980406973879783930289619240643955320574763282298513184754137066419829588200558889653756884714512569082291696579496147957009574856559159975430600764049595523214944721724618229087382990009118201426493970230094037871767314795807151411840859204603450495699032527972923029564045617141315511748646380995427511080164273051016861521306022130603410818165363571123776069334089378037317442063832513708").unwrap();
        let test_cap = Int::from_str("320855013829071061657328929876806521327").unwrap();
        let test_iterations: usize = 30;

        let proof = VDFProof::new(
            &rsa_int,
            &test_base,
            &VDFResult {
                result: test_result,
                iterations: test_iterations,
            },
            &test_cap,
        );

        let r = Int::one();
        let mut ebin = Int::one();
        let b = 2 * &r / &test_cap;

        ebin = &ebin.pow_mod(&Int::from(2), &rsa_int) * &test_base.pow_mod(&b, &rsa_int);
        ebin %= &rsa_int;

        assert_ne!(ebin, 1);
        assert_ne!(proof.proof, 1);
    }

    proptest! {
        #[test]
        fn works_with_any_prime_integer_as_cap(s in 0usize..usize::MAX) {
            let rsa_int: Int = Int::from_str(RSA_2048).unwrap();
            let s_int: Int = Int::from(s);
            if Verification::verify_safe_prime(s_int.clone()) {
                let root_hashed = util::hash(&Generator::new_safe_prime(64).to_string(), &rsa_int);
                let vdf = VDF::new(rsa_int, root_hashed, 3).with_cap(s_int.clone());
                println!("{:?}", vdf);
                let (_, receiver) = vdf.run_vdf_worker();
                if let Ok(res) = receiver.recv() {
                    if let Ok(proof) = res {
                        println!("{:?}", proof);
                        assert!(proof.verify());
                    }
                }
            }
        }
    }
}
