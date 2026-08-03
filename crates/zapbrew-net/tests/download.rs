//! Integration tests for `fetch_bottle` / `download_all`.
//!
//! Retry-heavy cases use a paused current-thread runtime so production
//! `2^attempt` sleeps auto-advance instantly (library is not built with
//! `cfg(test)` for integration tests).

use std::collections::HashMap;
use std::str::FromStr;
use std::time::Duration;

use camino::Utf8PathBuf;
use tempfile::TempDir;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};
use zapbrew_net::{CachedBottle, DownloadRequest, NetError, download_all, fetch_bottle};
use zapbrew_prefix::{CommandOutput, CommandRunner, CommandSpec, Env, EnvDetectInput};
use zapbrew_types::{BottleFile, BottleTag, Checksum, FormulaName, PkgVersion};

const BODY_HELLO: &[u8] = b"hello-bottle-bytes";
const SHA_HELLO: &str = "f6053eb591707fb2151494f9c893aeb7a90282435e167736661692dc2bfcf37b";

const BODY_PART1: &[u8] = b"PART1";
const BODY_PART2: &[u8] = b"PART2-REST-OF-FILE";
const SHA_COMBO: &str = "035785d101502f8fead9c13f50b0932aea3033e380c8424f26a9726d130d5baa";

const BODY_FULL: &[u8] = b"full-correct-bottle-payload!!";
const SHA_FULL: &str = "e68eb970c1fed43ccf56218c80710aa05abf1dea67ad1f8ac0e4de7ca855190f";

const BODY_WRONG: &[u8] = b"wrong-payload-xxxxx";
const SHA_WRONG: &str = "9d581018d7adf8d3f072b4eced5bc1e1a524ef718878aa48a8cf600087045831";

const BODY_C0: &[u8] = b"concurrent-0";
const SHA_C0: &str = "ef182f0882e1632061052b1154c2219c9906eb4b5e0393460c7a91cbbd8310f0";
const BODY_C1: &[u8] = b"concurrent-1";
const SHA_C1: &str = "b53eb7a3b27187f845a7c0ab558c09c4a28ec0f3f3a505297d6f31407062d30a";
const BODY_C2: &[u8] = b"concurrent-2";
const SHA_C2: &str = "678b8ed7e041c3e2ea2dbcbbc9340c5786643c412bbdf746ae2b974be57c8968";

struct PanicRunner;
impl CommandRunner for PanicRunner {
    fn run(&self, _spec: &CommandSpec) -> Result<CommandOutput, std::io::Error> {
        panic!("command runner should not be invoked for linux detect_from");
    }
}

fn test_env() -> (TempDir, Env) {
    let dir = TempDir::new().expect("tempdir");
    let home = Utf8PathBuf::from_path_buf(dir.path().to_path_buf()).expect("utf8");
    let cache = home.join("cache");
    let mut vars = HashMap::new();
    vars.insert("HOMEBREW_CACHE".to_owned(), cache.as_str().to_owned());
    vars.insert(
        "HOMEBREW_PREFIX".to_owned(),
        home.join("prefix").as_str().to_owned(),
    );
    let input = EnvDetectInput {
        os: "linux".to_owned(),
        arch: "x86_64".to_owned(),
        home,
        xdg_cache_home: None,
        vars,
        available_parallelism: 2,
    };
    let env = Env::detect_from(&input, &PanicRunner).expect("env");
    (dir, env)
}

fn bottle(url: &str, sha: &str) -> BottleFile {
    BottleFile {
        tag: BottleTag::from_str("x86_64_linux").expect("tag"),
        cellar: "any".into(),
        url: url.to_owned(),
        sha256: Checksum::from_str(sha).expect("sha"),
    }
}

fn name() -> FormulaName {
    FormulaName::from_str("wget").expect("name")
}

fn version() -> PkgVersion {
    PkgVersion::from_str("1.25.0").expect("version")
}

fn http() -> reqwest::Client {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::limited(10))
        .build()
        .expect("client")
}

fn blob_path(digest: &str) -> String {
    format!("/v2/homebrew/core/wget/blobs/sha256:{digest}")
}

fn blob_url(server: &MockServer, digest: &str) -> String {
    format!("{}{}", server.uri(), blob_path(digest))
}

fn assert_ok_cached(result: Result<CachedBottle, NetError>) -> CachedBottle {
    match result {
        Ok(cached) => cached,
        Err(err) => panic!("expected Ok, got {err}"),
    }
}

fn assert_err(result: Result<CachedBottle, NetError>) -> NetError {
    match result {
        Ok(cached) => panic!("expected Err, got Ok(reused={})", cached.reused),
        Err(err) => err,
    }
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn happy_path_writes_final_and_alias() {
    let server = MockServer::start().await;
    let digest = "abc";
    let url = blob_url(&server, digest);
    Mock::given(method("GET"))
        .and(path(blob_path(digest)))
        .and(header("authorization", "Bearer QQ=="))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(BODY_HELLO))
        .mount(&server)
        .await;

    let (_dir, env) = test_env();
    let bottle = bottle(&url, SHA_HELLO);
    let cached =
        assert_ok_cached(fetch_bottle(&env, &http(), &name(), &bottle, &version(), 0).await);

    assert!(!cached.reused);
    assert!(cached.path.is_file());
    assert!(cached.alias.exists());
    let bytes = std::fs::read(cached.path.as_std_path()).expect("read final");
    assert_eq!(bytes, BODY_HELLO);
    let target = std::fs::read_link(cached.alias.as_std_path()).expect("alias link");
    assert!(target.ends_with(cached.path.file_name().expect("file name")));
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn reuse_cached_valid_final_skips_network() {
    let server = MockServer::start().await;
    let digest = "reuse";
    let url = blob_url(&server, digest);
    Mock::given(method("GET"))
        .and(path(blob_path(digest)))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(BODY_HELLO))
        .expect(1)
        .mount(&server)
        .await;

    let (_dir, env) = test_env();
    let bottle = bottle(&url, SHA_HELLO);
    let client = http();

    let first =
        assert_ok_cached(fetch_bottle(&env, &client, &name(), &bottle, &version(), 0).await);
    assert!(!first.reused);

    let second =
        assert_ok_cached(fetch_bottle(&env, &client, &name(), &bottle, &version(), 0).await);
    assert!(second.reused);
    assert_eq!(second.path, first.path);
    assert_eq!(second.alias, first.alias);
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn auth_default_qq_when_no_tokens() {
    let server = MockServer::start().await;
    let digest = "authqq";
    let url = blob_url(&server, digest);
    Mock::given(method("GET"))
        .and(path(blob_path(digest)))
        .and(header("authorization", "Bearer QQ=="))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(BODY_HELLO))
        .expect(1)
        .mount(&server)
        .await;

    let (_dir, env) = test_env();
    assert!(env.docker_registry_token.is_none());
    assert!(env.github_packages_token.is_none());
    let bottle = bottle(&url, SHA_HELLO);
    let _ = assert_ok_cached(fetch_bottle(&env, &http(), &name(), &bottle, &version(), 0).await);
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn auth_docker_precedes_github() {
    let server = MockServer::start().await;
    let digest = "authdocker";
    let url = blob_url(&server, digest);
    Mock::given(method("GET"))
        .and(path(blob_path(digest)))
        .and(header("authorization", "Bearer docker-token"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(BODY_HELLO))
        .expect(1)
        .mount(&server)
        .await;

    let (_dir, mut env) = test_env();
    env.docker_registry_token = Some("docker-token".to_owned());
    env.github_packages_token = Some("github-token".to_owned());
    let bottle = bottle(&url, SHA_HELLO);
    let _ = assert_ok_cached(fetch_bottle(&env, &http(), &name(), &bottle, &version(), 0).await);
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn auth_github_when_no_docker() {
    let server = MockServer::start().await;
    let digest = "authgithub";
    let url = blob_url(&server, digest);
    Mock::given(method("GET"))
        .and(path(blob_path(digest)))
        .and(header("authorization", "Bearer github-token"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(BODY_HELLO))
        .expect(1)
        .mount(&server)
        .await;

    let (_dir, mut env) = test_env();
    env.github_packages_token = Some("github-token".to_owned());
    let bottle = bottle(&url, SHA_HELLO);
    let _ = assert_ok_cached(fetch_bottle(&env, &http(), &name(), &bottle, &version(), 0).await);
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn resume_206_matching_content_range() {
    let server = MockServer::start().await;
    let digest = "resume";
    let url = blob_url(&server, digest);
    let part1_len = BODY_PART1.len() as u64;
    let total = (BODY_PART1.len() + BODY_PART2.len()) as u64;
    let end = total - 1;

    let (_dir, env) = test_env();
    let bottle = bottle(&url, SHA_COMBO);
    let client = http();

    Mock::given(method("GET"))
        .and(path(blob_path(digest)))
        .respond_with(ResponseTemplate::new(200).set_body_bytes([BODY_PART1, BODY_PART2].concat()))
        .mount(&server)
        .await;

    let cached =
        assert_ok_cached(fetch_bottle(&env, &client, &name(), &bottle, &version(), 0).await);
    let final_path = cached.path.clone();
    let incomplete = Utf8PathBuf::from(format!("{final_path}.incomplete"));
    std::fs::remove_file(final_path.as_std_path()).expect("remove final");
    let _ = std::fs::remove_file(cached.alias.as_std_path());
    std::fs::write(incomplete.as_std_path(), BODY_PART1).expect("seed incomplete");

    server.reset().await;
    Mock::given(method("GET"))
        .and(path(blob_path(digest)))
        .and(header("range", format!("bytes={part1_len}-")))
        .respond_with(
            ResponseTemplate::new(206)
                .insert_header("content-range", format!("bytes {part1_len}-{end}/{total}"))
                .set_body_bytes(BODY_PART2),
        )
        .expect(1)
        .mount(&server)
        .await;

    let resumed =
        assert_ok_cached(fetch_bottle(&env, &client, &name(), &bottle, &version(), 0).await);
    assert!(!resumed.reused);
    let bytes = std::fs::read(resumed.path.as_std_path()).expect("read");
    assert_eq!(bytes, [BODY_PART1, BODY_PART2].concat());
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn restart_on_200_despite_range() {
    let server = MockServer::start().await;
    let digest = "restart";
    let url = blob_url(&server, digest);

    let (_dir, env) = test_env();
    let bottle = bottle(&url, SHA_FULL);
    let client = http();

    Mock::given(method("GET"))
        .and(path(blob_path(digest)))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(BODY_FULL))
        .mount(&server)
        .await;
    let cached =
        assert_ok_cached(fetch_bottle(&env, &client, &name(), &bottle, &version(), 0).await);
    let final_path = cached.path.clone();
    let incomplete = Utf8PathBuf::from(format!("{final_path}.incomplete"));
    std::fs::remove_file(final_path.as_std_path()).expect("remove final");
    let _ = std::fs::remove_file(cached.alias.as_std_path());
    std::fs::write(incomplete.as_std_path(), BODY_PART1).expect("seed partial incomplete");

    server.reset().await;
    // Incomplete still has PART1 (5 bytes), so the client must send Range; a 200
    // response must truncate/restart rather than append.
    Mock::given(method("GET"))
        .and(path(blob_path(digest)))
        .and(header("range", "bytes=5-"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(BODY_FULL))
        .expect(1)
        .mount(&server)
        .await;

    let restarted =
        assert_ok_cached(fetch_bottle(&env, &client, &name(), &bottle, &version(), 0).await);
    let bytes = std::fs::read(restarted.path.as_std_path()).expect("read");
    assert_eq!(bytes, BODY_FULL);
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn checksum_mismatch_retries_then_fails_with_exact_message() {
    let server = MockServer::start().await;
    let digest = "mismatch";
    let url = blob_url(&server, digest);

    Mock::given(method("GET"))
        .and(path(blob_path(digest)))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(BODY_WRONG))
        .expect(5)
        .mount(&server)
        .await;

    let (_dir, env) = test_env();
    let bottle = bottle(&url, SHA_HELLO);
    let err = assert_err(fetch_bottle(&env, &http(), &name(), &bottle, &version(), 0).await);

    match &err {
        NetError::ChecksumMismatch {
            expected,
            actual,
            path,
        } => {
            assert_eq!(expected.as_str(), SHA_HELLO);
            assert_eq!(actual.as_str(), SHA_WRONG);
            assert!(path.as_str().ends_with(".incomplete"));
            assert!(!path.exists(), "incomplete must be deleted after mismatch");
        }
        other => panic!("expected ChecksumMismatch, got {other}"),
    }

    let msg = err.to_string();
    assert!(msg.contains("SHA-256 mismatch"), "{msg}");
    assert!(msg.contains("Expected:"), "{msg}");
    assert!(msg.contains("Actual:"), "{msg}");
    assert!(msg.contains("File:"), "{msg}");
    assert!(
        msg.contains("To retry an incomplete download, remove the file above."),
        "{msg}"
    );
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn network_error_preserves_incomplete() {
    let server = MockServer::start().await;
    let digest = "neterr";
    let url = blob_url(&server, digest);

    let (_dir, env) = test_env();
    let bottle = bottle(&url, SHA_HELLO);
    let client = http();

    Mock::given(method("GET"))
        .and(path(blob_path(digest)))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(BODY_HELLO))
        .mount(&server)
        .await;
    let cached =
        assert_ok_cached(fetch_bottle(&env, &client, &name(), &bottle, &version(), 0).await);
    let final_path = cached.path.clone();
    let incomplete = Utf8PathBuf::from(format!("{final_path}.incomplete"));
    std::fs::remove_file(final_path.as_std_path()).expect("remove final");
    let _ = std::fs::remove_file(cached.alias.as_std_path());
    std::fs::write(incomplete.as_std_path(), BODY_PART1).expect("seed incomplete");
    assert!(incomplete.is_file());

    server.reset().await;
    Mock::given(method("GET"))
        .and(path(blob_path(digest)))
        .respond_with(ResponseTemplate::new(500))
        .expect(5)
        .mount(&server)
        .await;

    let err = assert_err(fetch_bottle(&env, &client, &name(), &bottle, &version(), 0).await);
    match err {
        NetError::InvalidResponse { .. } | NetError::Http { .. } => {}
        other => panic!("expected network/http-ish error, got {other}"),
    }
    assert!(
        incomplete.is_file(),
        "network failures must preserve .incomplete for resume"
    );
    let preserved = std::fs::read(incomplete.as_std_path()).expect("read incomplete");
    assert_eq!(preserved, BODY_PART1);
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn download_all_preserves_order_under_concurrency() {
    let server = MockServer::start().await;
    let (_dir, mut env) = test_env();
    env.download_concurrency = 2;

    let specs = [
        ("c0", BODY_C0, SHA_C0, Duration::from_millis(80)),
        ("c1", BODY_C1, SHA_C1, Duration::from_millis(10)),
        ("c2", BODY_C2, SHA_C2, Duration::from_millis(10)),
    ];

    let mut requests = Vec::new();
    for (digest, body, sha, delay) in specs {
        let url = blob_url(&server, digest);
        Mock::given(method("GET"))
            .and(path(blob_path(digest)))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_bytes(body)
                    .set_delay(delay),
            )
            .mount(&server)
            .await;
        requests.push(DownloadRequest {
            name: FormulaName::from_str(&format!("pkg-{digest}")).expect("name"),
            bottle: bottle(&url, sha),
            pkg_version: version(),
            rebuild: 0,
        });
    }

    let results = match download_all(&env, &http(), requests).await {
        Ok(results) => results,
        Err(err) => panic!("download_all failed: {err}"),
    };
    assert_eq!(results.len(), 3);
    let bodies: Vec<Vec<u8>> = results
        .iter()
        .map(|c| std::fs::read(c.path.as_std_path()).expect("read"))
        .collect();
    assert_eq!(bodies[0], BODY_C0);
    assert_eq!(bodies[1], BODY_C1);
    assert_eq!(bodies[2], BODY_C2);
}
