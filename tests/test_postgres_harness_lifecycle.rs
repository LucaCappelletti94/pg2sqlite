//! The PostgreSQL harness must not leak its container.
//!
//! The container lives in a `static`, and a static is never dropped, so the
//! `Drop` that would remove it never ran: every test binary left one running
//! forever, each with hundreds of `case_N` databases for autovacuum to grind
//! through. Nothing else cleans up either; testcontainers 0.27 has no reaper.
//!
//! Two guarantees, each proven against a real daemon:
//! - a binary that used the server removes its container when it exits, and
//! - a container an earlier run abandoned (a `SIGKILL` runs no exit code) is
//!   swept at the next start, while one a concurrent run is using is not.

mod helpers;

#[path = "helpers/postgres.rs"]
mod postgres_harness;

use std::process::Command;

/// Runs in a child process: touches the server, prints its container ID,
/// exits normally. Ignored so the parent test is the only one that runs it.
#[test]
#[ignore = "child probe for container_is_removed_when_the_binary_exits"]
fn probe_starts_server_and_exits() {
    let _connection = postgres_harness::fresh_database();
    println!("container-id={}", postgres_harness::container_id());
}

fn docker(args: &[&str]) -> std::process::Output {
    Command::new("docker").args(args).output().expect("run docker")
}

fn container_exists(id: &str) -> bool {
    docker(&["inspect", "--format", "{{.Id}}", id]).status.success()
}

/// The anonymous volumes the image's `VOLUME` declaration gave a container.
fn volumes_of(id: &str) -> Vec<String> {
    let out = docker(&[
        "inspect",
        "--format",
        "{{range .Mounts}}{{if eq .Type \"volume\"}}{{.Name}} {{end}}{{end}}",
        id,
    ]);
    String::from_utf8_lossy(&out.stdout).split_whitespace().map(str::to_owned).collect()
}

fn volume_exists(name: &str) -> bool {
    docker(&["volume", "inspect", name]).status.success()
}

/// Removes containers and volumes when dropped, so a failed assertion cannot
/// strand what the suite exists to prevent. `Drop` is infallible.
struct Cleanup {
    containers: Vec<String>,
    volumes: Vec<String>,
}

impl Drop for Cleanup {
    fn drop(&mut self) {
        for c in &self.containers {
            let _ = Command::new("docker").args(["rm", "-f", "-v", c]).output();
        }
        for v in &self.volumes {
            let _ = Command::new("docker").args(["volume", "rm", "-f", v]).output();
        }
    }
}

#[test]
fn container_is_removed_when_the_binary_exits() {
    let output = Command::new(std::env::current_exe().expect("own path"))
        .args(["--ignored", "--exact", "probe_starts_server_and_exits", "--nocapture"])
        .output()
        .expect("run the probe binary");
    assert!(output.status.success(), "probe failed: {}", String::from_utf8_lossy(&output.stderr));
    let stdout = String::from_utf8_lossy(&output.stdout);
    let id = stdout
        .lines()
        .find_map(|line| line.strip_prefix("container-id="))
        .expect("the probe printed its container id");
    assert!(!id.is_empty(), "probe printed an empty container id");
    assert!(
        !container_exists(id),
        "container {id} survived the exit of the binary that started it"
    );
}

#[test]
fn sweep_removes_abandoned_containers_and_keeps_live_ones() {
    let create = |started: &str| -> String {
        let out = docker(&[
            "create",
            "--label",
            &format!("{}=postgres", postgres_harness::CONTAINER_LABEL),
            "--label",
            &format!("{}={started}", postgres_harness::STARTED_LABEL),
            &format!("postgres:{}", postgres_harness::IMAGE_TAG),
        ]);
        assert!(out.status.success(), "docker create: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).trim().to_owned()
    };
    let abandoned = create("0");
    let cleanup = Cleanup {
        volumes: volumes_of(&abandoned),
        containers: vec![abandoned, create(&postgres_harness::now_secs().to_string())],
    };
    let [abandoned, live] = cleanup.containers.as_slice() else { unreachable!("two ids") };
    assert!(!cleanup.volumes.is_empty(), "the image declares a volume; none was created");

    postgres_harness::sweep_abandoned_containers();

    assert!(!container_exists(abandoned), "a container stamped at the epoch was not swept");
    let stranded: Vec<&String> = cleanup.volumes.iter().filter(|v| volume_exists(v)).collect();
    assert!(
        stranded.is_empty(),
        "the sweep left the swept container's volume behind: {stranded:?}"
    );
    assert!(
        container_exists(live),
        "a container stamped just now was swept: a sibling run would lose its server"
    );
}
