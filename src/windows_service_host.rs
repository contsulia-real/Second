use std::ffi::OsString;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use windows_service::service::{
    ServiceControl, ServiceControlAccept, ServiceExitCode, ServiceState, ServiceStatus, ServiceType,
};
use windows_service::service_control_handler::{
    self, ServiceControlHandlerResult, ServiceStatusHandle,
};
use windows_service::{define_windows_service, service_dispatcher};

struct Configuration {
    name: String,
    address: String,
    base: String,
    logs: PathBuf,
}
static CONFIG: OnceLock<Configuration> = OnceLock::new();

pub(crate) fn dispatch(name: &str, address: &str, base: &str, logs: &str) -> Result<(), String> {
    CONFIG
        .set(Configuration {
            name: name.to_owned(),
            address: address.to_owned(),
            base: base.to_owned(),
            logs: logs.into(),
        })
        .map_err(|_| "service configuration already initialized".to_owned())?;
    service_dispatcher::start(name, ffi_service_main)
        .map_err(|error| format!("SCM dispatcher failed: {error}"))
}

define_windows_service!(ffi_service_main, service_main);
fn service_main(_arguments: Vec<OsString>) {
    let Some(config) = CONFIG.get() else {
        return;
    };
    if let Err(error) = run(config) {
        let _ = log(&config.logs, &format!("SERVICE-FAILED {error}"));
    }
}

fn status(handle: &ServiceStatusHandle, state: ServiceState, failed: bool) -> Result<(), String> {
    handle
        .set_service_status(ServiceStatus {
            service_type: ServiceType::OWN_PROCESS,
            current_state: state,
            controls_accepted: if state == ServiceState::Running {
                ServiceControlAccept::STOP | ServiceControlAccept::SHUTDOWN
            } else {
                ServiceControlAccept::empty()
            },
            exit_code: ServiceExitCode::Win32(u32::from(failed)),
            checkpoint: if state == ServiceState::StartPending || state == ServiceState::StopPending
            {
                1
            } else {
                0
            },
            wait_hint: Duration::from_secs(30),
            process_id: None,
        })
        .map_err(|error| error.to_string())
}

fn run(config: &Configuration) -> Result<(), String> {
    let (sender, receiver) = tokio::sync::oneshot::channel();
    let sender = Mutex::new(Some(sender));
    let handle = service_control_handler::register(&config.name, move |event| match event {
        ServiceControl::Stop | ServiceControl::Shutdown => {
            if let Ok(mut sender) = sender.lock()
                && let Some(sender) = sender.take()
            {
                let _ = sender.send(());
            }
            ServiceControlHandlerResult::NoError
        }
        ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
        _ => ServiceControlHandlerResult::NotImplemented,
    })
    .map_err(|error| error.to_string())?;
    status(&handle, ServiceState::StartPending, false)?;
    let result = (|| {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .map_err(|e| e.to_string())?;
        runtime.block_on(crate::cli_node::node_with_shutdown(
            &config.address,
            &config.base,
            async {
                let _ = receiver.await;
                status(&handle, ServiceState::StopPending, false)
            },
            |line| {
                log(&config.logs, line)?;
                status(&handle, ServiceState::Running, false)
            },
        ))
    })();
    let _ = log(
        &config.logs,
        if result.is_ok() {
            "SERVICE-STOPPED"
        } else {
            "SERVICE-FAILED"
        },
    );
    status(&handle, ServiceState::Stopped, result.is_err())?;
    result
}

fn log(directory: &Path, message: &str) -> Result<(), String> {
    fs::create_dir_all(directory).map_err(|e| e.to_string())?;
    let path = directory.join("service.log");
    if fs::metadata(&path).is_ok_and(|m| m.len() > 1024 * 1024) {
        let backup = directory.join("service.previous.log");
        if backup.exists() {
            fs::remove_file(&backup).map_err(|e| e.to_string())?;
        }
        fs::rename(&path, backup).map_err(|e| e.to_string())?;
    }
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|e| e.to_string())?;
    let time = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_secs();
    writeln!(file, "{time} {message}").map_err(|e| e.to_string())
}
