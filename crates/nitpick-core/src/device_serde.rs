//! Serde forms of the types with invariants. Bytes are hex strings, so a
//! saved recovery record stays readable text.

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use super::{ContentHash, IccProfile};

impl Serialize for ContentHash {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        hex::serde::serialize(self.0, s)
    }
}

impl<'de> Deserialize<'de> for ContentHash {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        hex::serde::deserialize(d).map(Self)
    }
}

#[derive(Serialize, Deserialize)]
struct IccProfileForm {
    #[serde(with = "hex::serde")]
    bytes: Vec<u8>,
    hash: ContentHash,
}

impl Serialize for IccProfile {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        IccProfileForm {
            bytes: self.bytes.clone(),
            hash: self.hash,
        }
        .serialize(s)
    }
}

/// Repeats the [`IccProfile::new`] checks. A stored hash that differs from
/// the bytes fails: the record is corrupt, or another profile replaced it.
impl<'de> Deserialize<'de> for IccProfile {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let form = IccProfileForm::deserialize(d)?;
        let profile = Self::new(form.bytes).map_err(D::Error::custom)?;
        if profile.hash != form.hash {
            return Err(D::Error::custom(format!(
                "stored hash {} differs from profile hash {}",
                form.hash, profile.hash
            )));
        }
        Ok(profile)
    }
}
