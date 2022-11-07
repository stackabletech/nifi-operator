use std::{num::ParseFloatError, ops::Mul};

use snafu::{ResultExt, Snafu};
use stackable_operator::k8s_openapi::apimachinery::pkg::api::resource::Quantity;

#[derive(Debug, Snafu)]
#[snafu(module)]
pub enum FromK8sError {
    #[snafu(display("unknown unit {unit:?}"))]
    UnknownUnit { unit: String },
    #[snafu(display("failed to parse amount {amount:?}"))]
    UnparseableAmount {
        amount: String,
        source: ParseFloatError,
    },
}

pub struct StorageQuantity {
    mebibytes: f64,
}

impl StorageQuantity {
    pub fn from_k8s(quantity: &Quantity) -> Result<Self, FromK8sError> {
        use from_k8s_error::*;
        let start_of_unit = quantity.0.find(|chr: char| chr.is_alphabetic());
        let unit = start_of_unit.map_or("", |i| &quantity.0[i..]);
        let unit_factor = match unit {
            "Mi" => 1.0,
            "Gi" => 1024.0,
            unit => return UnknownUnitSnafu { unit }.fail(),
        };
        let amount = start_of_unit.map_or(quantity.0.as_ref(), |i| &quantity.0[..i]);
        let amount = amount
            .parse::<f64>()
            .context(UnparseableAmountSnafu { amount })?;
        Ok(Self {
            mebibytes: amount * unit_factor,
        })
    }

    pub fn to_nifi(&self) -> String {
        format!("{}MB", self.mebibytes)
    }
}

impl Mul<f64> for StorageQuantity {
    type Output = Self;

    fn mul(self, rhs: f64) -> Self::Output {
        Self {
            mebibytes: self.mebibytes * rhs,
        }
    }
}
