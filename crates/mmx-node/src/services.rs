//! Optional C++-style service startup from the same configuration layers.
//! Shutdown signals only children whose full process identity still matches.
use mmx_node::Result;
use std::{
    path::{Path, PathBuf},
    process::{Child, Command},
    thread,
    time::{Duration, Instant},
};
use vnx::config::Config;
struct Owned {
    child: Child,
    exe: PathBuf,
    cwd: PathBuf,
    argv: Vec<u8>,
    start: Option<String>,
}
fn start_time(pid: u32) -> Option<String> {
    std::fs::read_to_string(format!("/proc/{pid}/stat"))
        .ok()?
        .rsplit_once(") ")?
        .1
        .split_whitespace()
        .nth(19)
        .map(str::to_owned)
}
impl Owned {
    fn spawn(path: &Path, args: Vec<String>) -> Result<Self> {
        use std::os::unix::ffi::OsStrExt;
        let exe = std::fs::canonicalize(path)?;
        let cwd = std::env::current_dir()?.canonicalize()?;
        let mut argv = exe.as_os_str().as_bytes().to_vec();
        argv.push(0);
        for a in &args {
            argv.extend(a.as_bytes());
            argv.push(0);
        }
        let child = Command::new(&exe).args(&args).current_dir(&cwd).spawn()?;
        let start = start_time(child.id());
        Ok(Self {
            child,
            exe,
            cwd,
            argv,
            start,
        })
    }
    fn matches(&self) -> bool {
        let p = format!("/proc/{}", self.child.id());
        self.start.is_some()
            && start_time(self.child.id()) == self.start
            && std::fs::read_link(format!("{p}/exe")).ok().as_ref() == Some(&self.exe)
            && std::fs::read_link(format!("{p}/cwd")).ok().as_ref() == Some(&self.cwd)
            && std::fs::read(format!("{p}/cmdline")).ok().as_ref() == Some(&self.argv)
    }
    fn stop(&mut self) {
        if self.child.try_wait().ok().flatten().is_some() || !self.matches() {
            return;
        }
        let _ = Command::new("kill")
            .args(["-s", "INT", &self.child.id().to_string()])
            .status();
        let begin = Instant::now();
        while begin.elapsed() < Duration::from_secs(5) {
            if self.child.try_wait().ok().flatten().is_some() {
                return;
            }
            thread::sleep(Duration::from_millis(20));
        }
        if self.matches() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}
impl Drop for Owned {
    fn drop(&mut self) {
        self.stop();
    }
}
pub struct Services(Vec<Owned>);
impl Services {
    pub fn start(config: &Config, paths: &[PathBuf], node: &str, farmer: &str) -> Result<Self> {
        let directory = std::env::current_exe()?
            .parent()
            .ok_or("missing executable directory")?
            .to_owned();
        let mut base = vec!["-c".to_owned()];
        for path in paths {
            base.push(
                path.to_str()
                    .ok_or("configuration path is not UTF-8")?
                    .to_owned(),
            );
        }
        let mut services = Self(vec![]);
        let wallet = config.read::<bool>("wallet")?.unwrap_or(true);
        let farming = wallet && config.read::<bool>("farmer")?.unwrap_or(true);
        let wallet_endpoint = config
            .read::<String>("Wallet.listen")?
            .unwrap_or_else(|| "127.0.0.1:11335".into());
        if wallet {
            let mut args = base.clone();
            args.extend([
                "--node".into(),
                node.into(),
                "--listen".into(),
                wallet_endpoint.clone(),
            ]);
            services
                .0
                .push(Owned::spawn(&directory.join("mmx-wallet"), args)?);
        }
        if farming {
            let mut args = base.clone();
            args.extend([
                "--node".into(),
                node.into(),
                "--wallet".into(),
                wallet_endpoint,
                "--listen".into(),
                farmer.into(),
            ]);
            services
                .0
                .push(Owned::spawn(&directory.join("mmx-farmer"), args)?);
            if config.read::<bool>("harvester")?.unwrap_or(true) {
                let remote = config.read::<bool>("allow_remote")?.unwrap_or(false);
                let harvester_farmer = if remote {
                    config
                        .read::<String>("Farmer.harvester_endpoint")?
                        .unwrap_or_else(|| "127.0.0.1:11333".into())
                } else {
                    farmer.to_owned()
                };
                let mut args = base;
                args.extend(["--farmer".into(), harvester_farmer]);
                services
                    .0
                    .push(Owned::spawn(&directory.join("mmx-harvester"), args)?);
            }
        }
        Ok(services)
    }
    pub fn check(&mut self) -> Result<()> {
        for child in &mut self.0 {
            if let Some(status) = child.child.try_wait()? {
                return Err(format!("service {} exited: {status}", child.exe.display()).into());
            }
        }
        Ok(())
    }
    pub fn shutdown(&mut self) {
        for child in self.0.iter_mut().rev() {
            child.stop();
        }
    }
}
