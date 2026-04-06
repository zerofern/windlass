use tracing::warn;
use uom::si::f64::Information;
use uom::si::information::byte;

/// Returns available disk space at `path`. On error, returns `f64::MAX`
/// so the Core never triggers a false low-space alert.
pub fn check_disk_space(path: &str) -> Information {
    match available_bytes(path) {
        #[allow(clippy::cast_precision_loss)]
        // f64 mantissa is 52 bits (~8 PiB exact range). Practical disk sizes
        // are well under this, so the precision loss is negligible for alerting.
        Ok(bytes) => Information::new::<byte>(bytes as f64),
        Err(e) => {
            warn!("Failed to check disk space at {path}: {e}");
            Information::new::<byte>(f64::MAX)
        }
    }
}

fn available_bytes(path: &str) -> anyhow::Result<u64> {
    use std::ffi::CString;
    use std::mem::MaybeUninit;

    let c_path = CString::new(path)?;
    let mut stat = MaybeUninit::<libc::statvfs>::uninit();
    let ret = unsafe { libc::statvfs(c_path.as_ptr(), stat.as_mut_ptr()) };
    if ret != 0 {
        anyhow::bail!("statvfs failed: {}", std::io::Error::last_os_error());
    }
    let stat = unsafe { stat.assume_init() };
    Ok(stat.f_bavail * stat.f_frsize)
}

#[cfg(test)]
mod tests {
    use super::*;
    use uom::si::information::byte;

    #[test]
    fn check_disk_space_valid_path_returns_positive() {
        let space = check_disk_space("/tmp");
        let bytes = space.get::<byte>();
        assert!(bytes > 0.0, "expected positive space, got {bytes}");
        assert!(bytes < f64::MAX, "expected real value, not fallback MAX");
    }

    #[test]
    fn check_disk_space_invalid_path_returns_max() {
        let space = check_disk_space("/nonexistent/path/windlass_test");
        let bytes = space.get::<byte>();
        assert_eq!(bytes, f64::MAX);
    }
}
