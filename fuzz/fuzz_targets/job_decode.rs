//! `JobRequest::decode` and `JobResponse::decode` on arbitrary bytes: the
//! coverage worker and the client read each other's messages, and a worker
//! script cached from an older deployment may send anything. Decoding must
//! never panic, must refuse a message without the current version byte, and
//! anything that decodes must re-encode to bytes that decode to the same
//! bytes again (compared as bytes, since NaN is not equal to itself).
#![no_main]

use libfuzzer_sys::fuzz_target;
use observation_core::coverage_job::{JOB_VERSION, JobRequest, JobResponse};

fuzz_target!(|data: &[u8]| {
    let current = data.first() == Some(&JOB_VERSION);
    if let Ok(request) = JobRequest::decode(data) {
        assert!(current, "decoded a message of another version");
        let bytes = request.encode();
        assert_eq!(bytes.first(), Some(&JOB_VERSION));
        let again = JobRequest::decode(&bytes).expect("re-encoded request decodes");
        assert_eq!(again.encode(), bytes);
    }
    if let Ok(response) = JobResponse::decode(data) {
        assert!(current, "decoded a message of another version");
        let bytes = response.encode();
        assert_eq!(bytes.first(), Some(&JOB_VERSION));
        let again = JobResponse::decode(&bytes).expect("re-encoded response decodes");
        assert_eq!(again.encode(), bytes);
    }
});
