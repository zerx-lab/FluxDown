use super::{DATA_DIR, INTENT, RUNNING, lease, while_starting};
use std::fs::{self, File, TryLockError};
use std::io;

#[test]
fn portable_update_closes_admission_before_draining_running_processes() -> io::Result<()> {
    let dir = std::env::temp_dir().join(format!("fluxdown-lease-test-{}", std::process::id()));
    fs::create_dir(&dir)?;
    let result = (|| {
        File::create(dir.join("portable"))?;
        let exe = dir.join("fluxdown-agent.exe");
        let data = dir.join(DATA_DIR);
        let old = while_starting(&exe, |_| {
            let file = lease(&data, RUNNING)?;
            file.try_lock_shared().map_err(io::Error::from)?;
            Ok(file)
        })?
        .ok_or_else(|| io::Error::other("old process was not admitted"))?;
        let intent = lease(&data, INTENT)?;
        intent.try_lock().map_err(io::Error::from)?;
        let worker = lease(&data, RUNNING)?;
        assert!(matches!(worker.try_lock(), Err(TryLockError::WouldBlock)));
        assert_eq!(while_starting(&exe, |_| Ok("started"))?, None);
        drop(old);
        worker.try_lock().map_err(io::Error::from)?;
        drop(worker);
        drop(intent);
        assert_eq!(while_starting(&exe, |_| Ok("started"))?, Some("started"));
        Ok(())
    })();
    let cleanup = fs::remove_dir_all(dir);
    result.and(cleanup)
}
