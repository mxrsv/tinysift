use std::path::PathBuf;

use super::*;

const SECRET: &str = "sk-test-123";

/// A `KERN_PROCARGS2` buffer as the kernel lays it out: argc, executable path, NUL padding,
/// arguments, environment.
fn buffer(args: &[&str], env: &[&str]) -> Vec<u8> {
    let mut bytes = i32::try_from(args.len()).unwrap().to_ne_bytes().to_vec();
    bytes.extend_from_slice(b"/Applications/Google Chrome.app/Helper\0\0\0\0");
    for item in args.iter().chain(env) {
        bytes.extend_from_slice(item.as_bytes());
        bytes.push(0);
    }
    bytes
}

fn helper(flags: &[&str]) -> Option<ProcessRole> {
    let mut args = vec!["Google Chrome Helper (Aperitif Renderer)"];
    args.extend_from_slice(flags);
    parse_role(&buffer(&args, &[]))
}

#[test]
fn a_secret_in_the_command_line_never_reaches_the_output() {
    let secret_flag = format!("--api-key={SECRET}");
    let args = [
        "Google Chrome Helper (Aperitif Renderer)",
        "--type=renderer",
        &secret_flag,
        "--extension-process",
        "https://example.com/?token=sk-test-123",
    ];
    let role = parse_role(&buffer(&args, &["TOKEN=sk-test-123"]));
    assert_eq!(role, Some(ProcessRole::ExtensionRenderer));
    // The output is a fieldless enum: one byte, no room for any argument text.
    assert_eq!(std::mem::size_of::<Option<ProcessRole>>(), 1);
    assert!(!format!("{role:?}").contains(SECRET));
}

#[test]
fn chromium_types_and_sub_types_map_to_roles() {
    use ProcessRole::*;
    let cases: [(&[&str], Option<ProcessRole>); 11] = [
        (&["--type=renderer", "--lang=en"], Some(Renderer)),
        (
            &["--type=renderer", "--extension-process"],
            Some(ExtensionRenderer),
        ),
        (&["--type=gpu-process"], Some(Gpu)),
        (
            &[
                "--type=utility",
                "--utility-sub-type=network.mojom.NetworkService",
            ],
            Some(Network),
        ),
        (
            &[
                "--type=utility",
                "--utility-sub-type=storage.mojom.StorageService",
            ],
            Some(Storage),
        ),
        (
            &[
                "--type=utility",
                "--utility-sub-type=audio.mojom.AudioService",
            ],
            Some(AudioVideo),
        ),
        (
            &[
                "--type=utility",
                "--utility-sub-type=video_capture.mojom.VideoCaptureService",
            ],
            Some(AudioVideo),
        ),
        (
            &[
                "--type=utility",
                "--utility-sub-type=node.mojom.NodeService",
            ],
            Some(Utility),
        ),
        (&["--type=utility"], Some(Utility)),
        (&["--type=crashpad-handler"], Some(Utility)),
        (&["--lang=en"], None),
    ];
    for (flags, expected) in cases {
        assert_eq!(helper(flags), expected, "{flags:?}");
    }
}

#[test]
fn the_environment_and_malformed_buffers_give_no_role() {
    // `--type=gpu-process` sits in the environment, past argc: never read.
    let env_only = buffer(&["Helper", "--lang=en"], &["--type=gpu-process"]);
    assert_eq!(parse_role(&env_only), None);
    assert_eq!(parse_role(&[]), None);
    assert_eq!(parse_role(&[1, 0, 0]), None);
    assert_eq!(parse_role(&(-1i32).to_ne_bytes()), None);
    let mut truncated = 3i32.to_ne_bytes().to_vec();
    truncated.extend_from_slice(b"/bin/helper");
    assert_eq!(parse_role(&truncated), None);
}

fn info(pid: u32, name: &str, mine: bool) -> ProcessInfo {
    ProcessInfo {
        pid,
        name: name.into(),
        user: None,
        uid: None,
        is_current_user: mine,
        parent_pid: None,
        start_time: 100,
        cpu_percent: None,
        cpu_measured: false,
        memory_bytes: None,
        footprint_bytes: None,
        cpu_time_ns: None,
        responsible: None,
        exe: None,
    }
}

#[test]
fn webkit_and_decoder_services_are_named_by_executable() {
    let named = |exe: &str| {
        let mut process = info(u32::MAX, "short", true);
        process.exe = Some(PathBuf::from(format!("/System/Library/{exe}")));
        classify(&process)
    };
    assert_eq!(
        named("com.apple.WebKit.WebContent"),
        Some(ProcessRole::WebContent)
    );
    assert_eq!(
        named("com.apple.WebKit.WebContent.CaptivePortal"),
        Some(ProcessRole::WebContent)
    );
    assert_eq!(named("com.apple.WebKit.GPU"), Some(ProcessRole::WebGpu));
    assert_eq!(
        named("com.apple.WebKit.Networking"),
        Some(ProcessRole::WebNetworking)
    );
    assert_eq!(
        named("VTDecoderXPCService"),
        Some(ProcessRole::VideoDecoder)
    );
    // No executable path: the process name stands in.
    assert_eq!(
        classify(&info(u32::MAX, "com.apple.WebKit.GPU", true)),
        Some(ProcessRole::WebGpu)
    );
    // Another user's process has no role, whatever its name.
    assert_eq!(
        classify(&info(u32::MAX, "com.apple.WebKit.GPU", false)),
        None
    );
}

#[test]
fn the_cache_reads_each_identity_once_and_forgets_the_unseen() {
    let mut cache = RoleCache::default();
    let mut reads = Vec::new();
    let mut read = |p: &ProcessInfo| {
        reads.push(p.pid);
        Some(ProcessRole::Renderer)
    };
    let first = [info(1, "a", true), info(2, "b", true)];
    assert_eq!(cache.roles_with(&first, &mut read).len(), 2);
    cache.roles_with(&first, &mut read);
    // PID 1 restarted (new start time) and PID 2 left: 1 is read again, 2 is forgotten.
    let mut restarted = info(1, "a", true);
    restarted.start_time = 200;
    cache.roles_with(&[restarted], &mut read);
    cache.roles_with(&[info(2, "b", true)], &mut read);
    assert_eq!(reads, vec![1, 2, 1, 2]);
}

#[cfg(target_os = "macos")]
#[test]
fn a_live_childs_command_line_is_read_and_classified() {
    let mut child = std::process::Command::new("/bin/sh")
        .args(["-c", "sleep 30; true", "sh"])
        .args([
            "--type=utility",
            "--utility-sub-type=network.mojom.NetworkService",
        ])
        .arg(format!("--api-key={SECRET}"))
        .env("--type", "gpu-process")
        .spawn()
        .unwrap();
    let role = read_args_role(child.id());
    child.kill().unwrap();
    child.wait().unwrap();
    assert_eq!(role, Some(ProcessRole::Network));
    assert_eq!(read_args_role(std::process::id()), None);
    assert_eq!(read_args_role(0), None);
    assert_eq!(read_args_role(u32::MAX), None);
}

/// Cost of classifying 60 of this Mac's own processes, cold then cached (PD-U09). Run with
/// `cargo test -p tiny-core --lib role -- --ignored --nocapture`.
#[cfg(target_os = "macos")]
#[test]
#[ignore = "measurement on the live machine"]
fn measure_sixty_processes_cold_and_cached() {
    let mut processes: Vec<ProcessInfo> = crate::processes::Sampler::new()
        .sample()
        .processes
        .into_iter()
        .filter(|p| p.is_current_user)
        .collect();
    processes.sort_by_key(|p| !p.name.contains("Helper"));
    processes.truncate(60);
    let mut cache = RoleCache::default();
    let started = std::time::Instant::now();
    let roles = cache.roles(&processes);
    let cold = started.elapsed();
    let started = std::time::Instant::now();
    cache.roles(&processes);
    let cached = started.elapsed();
    let classified = roles.iter().filter(|role| role.is_some()).count();
    println!(
        "{} processes ({classified} with a role): cold {cold:?}, cached {cached:?}",
        processes.len()
    );
}
