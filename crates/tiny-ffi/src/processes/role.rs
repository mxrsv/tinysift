//! The process role crossing the boundary. Filled only by `process_list_for`.

use tiny_core::processes::ProcessRole;

/// What a helper process does for its app. Fieldless, like the core role: no argument text
/// ever crosses the boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum FfiProcessRole {
    Renderer,
    ExtensionRenderer,
    Gpu,
    Network,
    Storage,
    AudioVideo,
    Utility,
    WebContent,
    WebGpu,
    WebNetworking,
    VideoDecoder,
}

impl From<ProcessRole> for FfiProcessRole {
    fn from(role: ProcessRole) -> Self {
        match role {
            ProcessRole::Renderer => Self::Renderer,
            ProcessRole::ExtensionRenderer => Self::ExtensionRenderer,
            ProcessRole::Gpu => Self::Gpu,
            ProcessRole::Network => Self::Network,
            ProcessRole::Storage => Self::Storage,
            ProcessRole::AudioVideo => Self::AudioVideo,
            ProcessRole::Utility => Self::Utility,
            ProcessRole::WebContent => Self::WebContent,
            ProcessRole::WebGpu => Self::WebGpu,
            ProcessRole::WebNetworking => Self::WebNetworking,
            ProcessRole::VideoDecoder => Self::VideoDecoder,
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::TinySession;

    use super::FfiProcessRole;

    #[test]
    fn the_full_list_has_no_roles_and_the_pid_list_reads_a_child_role() {
        let session = TinySession::new();
        assert!(session
            .process_list()
            .unwrap()
            .processes
            .iter()
            .all(|p| p.role.is_none()));
        let mut child = std::process::Command::new("/bin/sh")
            .args(["-c", "sleep 30; true", "sh", "--type=gpu-process"])
            .spawn()
            .unwrap();
        let own = std::process::id();
        let read = session.process_list_for(vec![child.id(), own]);
        let again = session.process_list_for(vec![child.id()]);
        child.kill().unwrap();
        child.wait().unwrap();
        let role_of = |pid| read.iter().find(|p| p.pid == pid).map(|p| p.role);
        if cfg!(target_os = "macos") {
            assert_eq!(role_of(child.id()), Some(Some(FfiProcessRole::Gpu)));
            assert_eq!(again[0].role, Some(FfiProcessRole::Gpu));
        }
        assert_eq!(role_of(own), Some(None));
    }
}
