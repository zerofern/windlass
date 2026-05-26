use tokio::sync::mpsc::UnboundedSender;

use windlass_local::{docker::DockerClient, vpn_files};
use windlass_machine::{Shell, Timed};
use windlass_vpn_core::{VpnAction, VpnEvent};

pub struct VpnShellConfig {
    pub docker: DockerClient,
    pub vpn_ip_file: String,
    pub vpn_port_file: String,
}

pub struct VpnShell {
    docker: DockerClient,
    vpn_ip_file: String,
    vpn_port_file: String,
}

impl Shell for VpnShell {
    type Config = VpnShellConfig;
    type Event = VpnEvent;
    type Action = VpnAction;

    async fn new(config: Self::Config, _event_tx: UnboundedSender<Timed<VpnEvent>>) -> Self {
        Self {
            docker: config.docker,
            vpn_ip_file: config.vpn_ip_file,
            vpn_port_file: config.vpn_port_file,
        }
    }

    fn dispatch(&mut self, action: VpnAction, event_tx: &UnboundedSender<Timed<VpnEvent>>) {
        match action {
            VpnAction::StartMonitoring => {}
            VpnAction::InspectContainer => {
                let docker = self.docker.clone();
                let tx = event_tx.clone();
                tokio::spawn(async move {
                    let event = if docker.is_gluetun_healthy().await {
                        VpnEvent::ContainerHealthy
                    } else {
                        VpnEvent::ContainerUnhealthy
                    };
                    let _ = tx.send(Timed::now(event));
                });
            }
            VpnAction::ReadPortFiles => {
                let ip_file = self.vpn_ip_file.clone();
                let port_file = self.vpn_port_file.clone();
                let tx = event_tx.clone();
                tokio::spawn(async move {
                    let result = tokio::task::spawn_blocking(move || {
                        vpn_files::read_port_files(&ip_file, &port_file)
                    })
                    .await
                    .unwrap_or_else(|e| Err(e.to_string()));
                    let event = match result {
                        Ok((_, port)) => VpnEvent::PortFileChanged { port },
                        Err(reason) => VpnEvent::StateReadFailed { reason },
                    };
                    let _ = tx.send(Timed::now(event));
                });
            }
            VpnAction::ScheduleTimer { timer, after } => {
                let tx = event_tx.clone();
                tokio::spawn(async move {
                    tokio::time::sleep(after).await;
                    let _ = tx.send(Timed::now(VpnEvent::TimerFired(timer)));
                });
            }
        }
    }
}
