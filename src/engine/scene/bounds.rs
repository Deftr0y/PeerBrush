//! Source36-specific sequence bounds, checked before collecting excess elements.
use serde::{
    de::{DeserializeSeed, Error, SeqAccess, Visitor},
    Deserialize, Deserializer,
};
use std::fmt;

const CONTOURS: usize = 500_000;
const VERTICES: usize = 500_000;
const POLYGON: usize = 8192;

pub(super) fn contours<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<Vec<[f32; 2]>>, D::Error> {
    Contours {
        maximum: CONTOURS,
        vertices: VERTICES,
    }
    .deserialize(d)
}
pub(super) fn polygon<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Vec<[f32; 2]>>, D::Error> {
    struct Polygon(Vec<[f32; 2]>);
    impl<'de> Deserialize<'de> for Polygon {
        fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
            let mut remaining = POLYGON;
            Points {
                remaining: &mut remaining,
            }
            .deserialize(d)
            .map(Polygon)
        }
    }
    Option::<Polygon>::deserialize(d).map(|value| value.map(|v| v.0))
}
struct Contours {
    maximum: usize,
    vertices: usize,
}
impl<'de> DeserializeSeed<'de> for Contours {
    type Value = Vec<Vec<[f32; 2]>>;
    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<Self::Value, D::Error> {
        d.deserialize_seq(self)
    }
}
impl<'de> Visitor<'de> for Contours {
    type Value = Vec<Vec<[f32; 2]>>;
    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("bounded Source36 contours")
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
        let mut output = Vec::new();
        let mut remaining = self.vertices;
        loop {
            let full = output.len() == self.maximum;
            let next = seq.next_element_seed(Contour {
                full,
                remaining: &mut remaining,
            })?;
            let Some(points) = next else {
                break;
            };
            output
                .try_reserve(1)
                .map_err(|_| A::Error::custom("Source36 contour allocation refused"))?;
            output.push(points);
        }
        Ok(output)
    }
}
struct Contour<'a> {
    full: bool,
    remaining: &'a mut usize,
}
impl<'de> DeserializeSeed<'de> for Contour<'_> {
    type Value = Vec<[f32; 2]>;
    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<Self::Value, D::Error> {
        if self.full {
            return Err(D::Error::custom("Too many Source36 contour arrays"));
        }
        Points {
            remaining: self.remaining,
        }
        .deserialize(d)
    }
}
struct Points<'a> {
    remaining: &'a mut usize,
}
impl<'de> DeserializeSeed<'de> for Points<'_> {
    type Value = Vec<[f32; 2]>;
    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<Self::Value, D::Error> {
        d.deserialize_seq(self)
    }
}
impl<'de> Visitor<'de> for Points<'_> {
    type Value = Vec<[f32; 2]>;
    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("bounded finite Source36 points")
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
        let mut output = Vec::new();
        while let Some(point) = seq.next_element_seed(Point {
            remaining: &mut *self.remaining,
        })? {
            output
                .try_reserve(1)
                .map_err(|_| A::Error::custom("Source36 point allocation refused"))?;
            output.push(point);
        }
        Ok(output)
    }
}
struct Point<'a> {
    remaining: &'a mut usize,
}
impl<'de> DeserializeSeed<'de> for Point<'_> {
    type Value = [f32; 2];
    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<Self::Value, D::Error> {
        if *self.remaining == 0 {
            return Err(D::Error::custom("Too many Source36 contour vertices"));
        }
        let point = <[f32; 2]>::deserialize(d)?;
        if point.iter().any(|n| !n.is_finite() || n.abs() > 100_000.) {
            return Err(D::Error::custom("Invalid Source36 contour coordinate"));
        }
        *self.remaining -= 1;
        Ok(point)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_excess_array_before_its_payload_and_counts_empty_arrays() {
        let mut d = serde_json::Deserializer::from_str("[[],[],[\"not a point\"]]");
        let error = Contours {
            maximum: 2,
            vertices: 5,
        }
        .deserialize(&mut d)
        .unwrap_err()
        .to_string();
        assert!(error.contains("Too many Source36 contour arrays"));
    }
    #[test]
    fn vertices_are_global_across_contours_and_refused_before_excess_payload() {
        let mut d = serde_json::Deserializer::from_str("[[[1,2]],[[3,4],[\"not a point\"]]]");
        let error = Contours {
            maximum: 5,
            vertices: 2,
        }
        .deserialize(&mut d)
        .unwrap_err()
        .to_string();
        assert!(error.contains("Too many Source36 contour vertices"));
        let mut d = serde_json::Deserializer::from_str("[[[1,2]],[],[[3,4]]]");
        assert_eq!(
            Contours {
                maximum: 3,
                vertices: 2
            }
            .deserialize(&mut d)
            .unwrap()
            .len(),
            3
        );
    }
}
