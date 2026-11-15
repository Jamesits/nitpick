use super::*;

#[test]
fn error_statistics() {
    assert_eq!(ErrorStats::new(Vec::new()), Ok(None));
    assert_eq!(
        ErrorStats::new(vec![3.0, -1.0, 2.0]),
        Ok(Some(ErrorStats {
            count: 3,
            max: 3.0,
            median: 2.0
        }))
    );
    assert_eq!(
        ErrorStats::new(vec![4.0, 1.0, 2.0, 8.0]),
        Ok(Some(ErrorStats {
            count: 4,
            max: 8.0,
            median: 3.0
        }))
    );
    assert!(ErrorStats::new(vec![1.0, f64::NAN]).is_err());
    assert!(ErrorStats::new(vec![f64::INFINITY]).is_err());
}
