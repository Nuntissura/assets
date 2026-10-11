use crate::Error;

/// The host supplies durable lineage and a fresh incarnation for each Core instance.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(deny_unknown_fields))]
pub struct Identity {
    pub lineage: String,
    pub incarnation: String,
}
impl Identity {
    pub fn new(lineage: impl Into<String>, incarnation: impl Into<String>) -> Result<Self, Error> {
        let identity = Self {
            lineage: lineage.into(),
            incarnation: incarnation.into(),
        };
        identity.validate()?;
        Ok(identity)
    }
    pub(crate) fn validate(&self) -> Result<(), Error> {
        if [&self.lineage, &self.incarnation]
            .iter()
            .any(|s| s.is_empty() || s.len() > 128)
        {
            return Err(Error::InvalidIdentity);
        }
        Ok(())
    }
}
