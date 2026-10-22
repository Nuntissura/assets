use crate::{Error, Snapshot};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClientClass {
    B2C,
    B2B,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClientPricing {
    Standard,
    Tier(String),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClientTerms {
    pub classification: ClientClass,
    pub pricing: ClientPricing,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PriceBasis {
    B2C,
    B2B,
    Custom(String),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TaxBasis {
    Included,
    Excluded,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PricingTier {
    pub id: String,
    pub name: String,
    pub base: PriceBasis,
    /// One additional percentage discount: 100 basis points = 1 percent.
    pub discount_basis_points: u16,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Amount(#[serde(with = "crate::contracts::decimal_integer")] pub i64);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PriceBook {
    pub currency: String,
    pub scale: u8,
    pub tax_basis: TaxBasis,
    pub b2c: Option<Amount>,
    pub b2b: Option<Amount>,
    pub custom: BTreeMap<String, Amount>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Quote {
    pub client_id: String,
    pub product_id: String,
    pub classification: ClientClass,
    pub tier: Option<String>,
    pub base: PriceBasis,
    pub currency: String,
    pub scale: u8,
    pub tax_basis: TaxBasis,
    pub base_amount: Amount,
    pub discount_amount: Amount,
    pub final_amount: Amount,
    pub discount_basis_points: u16,
    pub client_revision: u64,
    pub product_revision: u64,
    pub workspace_revision: u64,
}

pub(crate) fn validate_book(book: &PriceBook) -> Result<(), Error> {
    if book.currency.len() != 3
        || !book.currency.bytes().all(|b| b.is_ascii_uppercase())
        || book.scale > 6
    {
        return Err(Error::Invalid("currency/scale"));
    }
    if book.custom.len() > 32 {
        return Err(Error::Capacity("custom prices"));
    }
    if book.b2c.is_none() && book.b2b.is_none() && book.custom.is_empty() {
        return Err(Error::Invalid("empty price book"));
    }
    for amount in book
        .b2c
        .iter()
        .chain(book.b2b.iter())
        .chain(book.custom.values())
    {
        if amount.0 < 0 {
            return Err(Error::Invalid("negative price"));
        }
    }
    Ok(())
}

pub(crate) fn calculate(
    state: &Snapshot,
    client_id: &str,
    product_id: &str,
) -> Result<Quote, Error> {
    let client = state.records.get(client_id).ok_or(Error::NotFound)?;
    let product = state.records.get(product_id).ok_or(Error::NotFound)?;
    let terms = client
        .client_terms
        .as_ref()
        .ok_or(Error::Invalid("client classification required"))?;
    let book = product
        .prices
        .as_ref()
        .ok_or(Error::Invalid("product pricing required"))?;
    validate_book(book)?;
    let (base, discount, tier) = match &terms.pricing {
        ClientPricing::Standard => (
            match terms.classification {
                ClientClass::B2C => PriceBasis::B2C,
                ClientClass::B2B => PriceBasis::B2B,
            },
            0,
            None,
        ),
        ClientPricing::Tier(id) => {
            let tier = state.pricing_tiers.get(id).ok_or(Error::NotFound)?;
            (
                tier.base.clone(),
                tier.discount_basis_points,
                Some(tier.name.clone()),
            )
        }
    };
    let amount = match &base {
        PriceBasis::B2C => book.b2c.as_ref(),
        PriceBasis::B2B => book.b2b.as_ref(),
        PriceBasis::Custom(key) => book.custom.get(key),
    }
    .ok_or(Error::Invalid("selected base price unavailable"))?;
    if discount > 10000 {
        return Err(Error::Invalid("discount range"));
    }
    // Round the discount half-up to the selected currency's minor unit.
    let discount_amount = ((amount.0 as u128 * discount as u128 + 5000) / 10000) as i64;
    Ok(Quote {
        client_id: client_id.into(),
        product_id: product_id.into(),
        classification: terms.classification.clone(),
        tier,
        base,
        currency: book.currency.clone(),
        scale: book.scale,
        tax_basis: book.tax_basis.clone(),
        base_amount: amount.clone(),
        discount_amount: Amount(discount_amount),
        final_amount: Amount(amount.0 - discount_amount),
        discount_basis_points: discount,
        client_revision: client.revision,
        product_revision: product.revision,
        workspace_revision: state.revision,
    })
}
