/// Interpolacja liniowa krzywej temperatura -> procent obrotów.
/// `points` to pary [temperatura °C, procent], posortowane rosnąco po temperaturze.
pub fn eval(points: &[[f32; 2]], temp: f32) -> f32 {
    let (Some(first), Some(last)) = (points.first(), points.last()) else {
        return 100.0;
    };
    if temp <= first[0] {
        return first[1];
    }
    if temp >= last[0] {
        return last[1];
    }
    for w in points.windows(2) {
        let (a, b) = (w[0], w[1]);
        if temp >= a[0] && temp <= b[0] {
            let span = b[0] - a[0];
            if span <= f32::EPSILON {
                return b[1];
            }
            return a[1] + (b[1] - a[1]) * (temp - a[0]) / span;
        }
    }
    last[1]
}

#[cfg(test)]
mod tests {
    use super::eval;

    #[test]
    fn interpolates_and_clamps() {
        let p = [[30.0, 20.0], [50.0, 40.0], [80.0, 100.0]];
        assert_eq!(eval(&p, 10.0), 20.0);
        assert_eq!(eval(&p, 40.0), 30.0);
        assert_eq!(eval(&p, 65.0), 70.0);
        assert_eq!(eval(&p, 95.0), 100.0);
    }
}
