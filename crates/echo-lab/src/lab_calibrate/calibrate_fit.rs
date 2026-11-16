use serde::Serialize;

#[derive(Debug, Serialize)]
pub(super) struct AxisFit {
    pub scale: f64,
    pub offset: f64,
    pub max_residual_px: f64,
}

pub(super) fn axis(points: &[(f64, f64)]) -> Result<AxisFit, String> {
    let count = f64::from(u32::try_from(points.len()).map_err(|_| "calibrate: too many fit points")?);
    let mean_x = points.iter().map(|p| p.0).sum::<f64>() / count;
    let mean_y = points.iter().map(|p| p.1).sum::<f64>() / count;
    let variance = points.iter().map(|p| (p.0 - mean_x).powi(2)).sum::<f64>();
    let covariance = points.iter().map(|p| (p.0 - mean_x) * (p.1 - mean_y)).sum::<f64>();
    let scale = covariance / variance;
    let offset = mean_y - scale * mean_x;
    if !scale.is_finite() || !offset.is_finite() || variance == 0.0 {
        return Err("calibrate: cannot fit axis".into());
    }
    let max_residual_px = points
        .iter()
        .map(|p| (scale.mul_add(p.0, offset) - p.1).abs())
        .fold(0.0, f64::max);
    Ok(AxisFit {
        scale,
        offset,
        max_residual_px,
    })
}

pub(super) fn acceptable(residual: f64) -> Result<(), String> {
    if !residual.is_finite() || residual > 24.0 {
        return Err("calibrate: max residual exceeds 24 px".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_and_scaled_axes() {
        for maximum in [1280.0, 800.0] {
            let points = [(0.0, 0.0), (maximum, maximum), (maximum / 2.0, maximum / 2.0)];
            let fit = axis(&points).unwrap();
            assert_eq!((fit.scale, fit.offset, fit.max_residual_px), (1.0, 0.0, 0.0));
            let fit = axis(&[(0.0, 0.0), (maximum - 1.0, maximum)]).unwrap();
            assert!((fit.scale - maximum / (maximum - 1.0)).abs() < 1e-9);
            assert!(fit.offset.abs() < 1e-9);
            assert!(fit.max_residual_px < 1e-9);
        }
    }

    #[test]
    fn residual_boundary_and_singular_fits() {
        let fit = axis(&[
            (64.0, 64.0),
            (1216.0, 1216.0),
            (1216.0, 1216.0),
            (64.0, 64.0),
            (680.0, 640.0),
        ])
        .unwrap();
        assert!((fit.max_residual_px - 165_888.0 / 5189.0).abs() < 1e-9);
        assert_eq!(
            acceptable(fit.max_residual_px).unwrap_err(),
            "calibrate: max residual exceeds 24 px"
        );
        assert_eq!(acceptable(24.0), Ok(()));
        assert!(acceptable(24.00001).is_err());
        assert!(acceptable(f64::NAN).is_err());
        for points in [&[][..], &[(1.0, 1.0)][..], &[(1.0, 1.0), (1.0, 2.0)][..]] {
            assert_eq!(axis(points).unwrap_err(), "calibrate: cannot fit axis");
        }
    }
}
