//! Coordinates, and the shift China puts on them.
//!
//! Mainland China publishes maps in GCJ-02 ("Mars coordinates"): the same
//! place has one pair of numbers in the raw GPS/WGS-84 frame every device and
//! every non-Chinese map uses, and a different pair - shifted by up to ~600m -
//! on Chinese maps. Anything that carries a position between the two has to
//! convert, which is what this module is for:
//!
//! * the map picker draws OpenStreetMap tiles, which are WGS-84, and the phone
//!   must be told WGS-84 (that is what a real GPS reports, and Chinese apps
//!   apply their own shift);
//! * the "open in Tencent Maps" link is a Chinese map, so it needs GCJ-02.
//!
//! The algorithm is the well-known public one: a fixed polynomial
//! approximation of the offset, applied only inside China's bounding box -
//! outside it there is no shift, and applying one would put the marker in the
//! sea.

/// Semi-major axis of the Krasovsky 1940 ellipsoid the offset was fitted to.
const A: f64 = 6_378_245.0;
/// Its squared eccentricity.
const EE: f64 = 0.006_693_421_622_965_943;

/// Whether a point is outside the area the shift applies to.
///
/// The bounding box is the usual one (a rectangle round mainland China); it is
/// deliberately generous, because a point just outside it that still gets
/// shifted is a few hundred metres off, while a point inside that does not is
/// the same - and the box only has to be right for places people pick on a map.
fn out_of_china(lat: f64, lon: f64) -> bool {
    !(73.66..=135.05).contains(&lon) || !(3.86..=53.55).contains(&lat)
}

fn transform_lat(x: f64, y: f64) -> f64 {
    let mut ret = -100.0 + 2.0 * x + 3.0 * y + 0.2 * y * y + 0.1 * x * y + 0.2 * x.abs().sqrt();
    ret += (20.0 * (6.0 * x * std::f64::consts::PI).sin()
        + 20.0 * (2.0 * x * std::f64::consts::PI).sin())
        * 2.0
        / 3.0;
    ret += (20.0 * (y * std::f64::consts::PI).sin()
        + 40.0 * (y / 3.0 * std::f64::consts::PI).sin())
        * 2.0
        / 3.0;
    ret += (160.0 * (y / 12.0 * std::f64::consts::PI).sin()
        + 320.0 * (y * std::f64::consts::PI / 30.0).sin())
        * 2.0
        / 3.0;
    ret
}

fn transform_lon(x: f64, y: f64) -> f64 {
    let mut ret = 300.0 + x + 2.0 * y + 0.1 * x * x + 0.1 * x * y + 0.1 * x.abs().sqrt();
    ret += (20.0 * (6.0 * x * std::f64::consts::PI).sin()
        + 20.0 * (2.0 * x * std::f64::consts::PI).sin())
        * 2.0
        / 3.0;
    ret += (20.0 * (x * std::f64::consts::PI).sin()
        + 40.0 * (x / 3.0 * std::f64::consts::PI).sin())
        * 2.0
        / 3.0;
    ret += (150.0 * (x / 12.0 * std::f64::consts::PI).sin()
        + 300.0 * (x / 30.0 * std::f64::consts::PI).sin())
        * 2.0
        / 3.0;
    ret
}

/// The shift, in degrees, at a WGS-84 point inside China.
fn delta(lat: f64, lon: f64) -> (f64, f64) {
    let d_lat = transform_lat(lon - 105.0, lat - 35.0);
    let d_lon = transform_lon(lon - 105.0, lat - 35.0);
    let rad_lat = lat / 180.0 * std::f64::consts::PI;
    let magic = rad_lat.sin();
    let magic = 1.0 - EE * magic * magic;
    let sqrt_magic = magic.sqrt();
    let d_lat = (d_lat * 180.0) / ((A * (1.0 - EE)) / (magic * sqrt_magic) * std::f64::consts::PI);
    let d_lon = (d_lon * 180.0) / (A / sqrt_magic * rad_lat.cos() * std::f64::consts::PI);
    (d_lat, d_lon)
}

/// WGS-84 (what the device and OpenStreetMap use) → GCJ-02 (Chinese maps).
pub fn wgs84_to_gcj02(lat: f64, lon: f64) -> (f64, f64) {
    if out_of_china(lat, lon) {
        return (lat, lon);
    }
    let (d_lat, d_lon) = delta(lat, lon);
    (lat + d_lat, lon + d_lon)
}

/// GCJ-02 (Chinese maps) → WGS-84 (what the device and OpenStreetMap use).
///
/// The forward shift has no closed-form inverse, so it is inverted by
/// iteration: guess, measure how far the shifted guess lands from the target,
/// correct by that much, repeat. It converges in a few rounds - the shift is
/// smooth and changes by well under a metre across a metre.
pub fn gcj02_to_wgs84(lat: f64, lon: f64) -> (f64, f64) {
    if out_of_china(lat, lon) {
        return (lat, lon);
    }
    let (mut w_lat, mut w_lon) = (lat, lon);
    for _ in 0..12 {
        let (d_lat, d_lon) = delta(w_lat, w_lon);
        let next = (lat - d_lat, lon - d_lon);
        // 1e-9 degrees is about a tenth of a millimetre: further rounds only
        // spend floating-point noise.
        if (next.0 - w_lat).abs() < 1e-9 && (next.1 - w_lon).abs() < 1e-9 {
            return next;
        }
        (w_lat, w_lon) = next;
    }
    (w_lat, w_lon)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The offset is the whole point of this module, so its magnitude is
    /// asserted rather than assumed: in Beijing it is a few hundred metres,
    /// which is what makes a Chinese map's marker sit on the wrong street
    /// without the conversion.
    #[test]
    fn the_shift_in_beijing_is_the_usual_few_hundred_metres() {
        let (lat, lon) = wgs84_to_gcj02(39.90733, 116.39117);
        let d_lat = lat - 39.90733;
        let d_lon = lon - 116.39117;
        assert!(
            (0.0005..0.0030).contains(&d_lat),
            "latitude shift {d_lat} out of range"
        );
        assert!(
            (0.0040..0.0080).contains(&d_lon),
            "longitude shift {d_lon} out of range"
        );
    }

    /// Round-tripping is the property that matters for the two uses: pick on
    /// the map (WGS-84), hand the link a GCJ-02 pair, and the same spot comes
    /// back when the pair is converted again.
    #[test]
    fn converting_there_and_back_returns_the_same_place() {
        for (lat, lon) in [
            (39.90733, 116.39117), // Beijing
            (31.23037, 121.47370), // Shanghai
            (23.12911, 113.26438), // Guangzhou
            (22.54309, 114.05787), // Shenzhen
            (43.82559, 87.61684),  // Ürümqi, far from the polynomial's centre
        ] {
            let (g_lat, g_lon) = wgs84_to_gcj02(lat, lon);
            let (b_lat, b_lon) = gcj02_to_wgs84(g_lat, g_lon);
            assert!(
                (b_lat - lat).abs() < 1e-6 && (b_lon - lon).abs() < 1e-6,
                "{lat},{lon} came back as {b_lat},{b_lon}"
            );
        }
    }

    /// Outside China there is no shift at all - a marker in Tokyo or New York
    /// must not be moved, and Google's link (which takes WGS-84) must get the
    /// numbers the user picked.
    #[test]
    fn nothing_outside_china_is_moved() {
        for (lat, lon) in [(35.6762, 139.6503), (40.7128, -74.0060), (48.8566, 2.3522)] {
            assert_eq!(wgs84_to_gcj02(lat, lon), (lat, lon));
            assert_eq!(gcj02_to_wgs84(lat, lon), (lat, lon));
        }
    }

    /// The box is a rectangle round mainland China, and it does not follow the
    /// border: Hong Kong and Taipei are both inside it and so both get the
    /// shift, while in practice neither is mapped that way. This documents the
    /// known limitation rather than pretending it is exact - it is what every
    /// implementation of this algorithm does, and the cost is a few hundred
    /// metres on a map link. If it ever matters, the box needs to exclude them
    /// explicitly.
    #[test]
    fn the_bounding_box_is_a_box_not_a_border() {
        assert!(!out_of_china(22.3193, 114.1694)); // Hong Kong: inside the box
        assert!(!out_of_china(25.0330, 121.5654)); // Taipei: inside it too
        assert!(out_of_china(35.6762, 139.6503)); // Tokyo: outside, as intended
    }
}
