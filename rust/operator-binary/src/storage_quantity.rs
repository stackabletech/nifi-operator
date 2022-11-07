use std::ops::Mul;

use stackable_operator::k8s_openapi::apimachinery::pkg::api::resource::Quantity;

pub struct StorageQuantity {
    mebibytes: f64,
}

impl StorageQuantity {
    pub fn from_k8s(quantity: &Quantity) -> Self {
        let start_of_unit = quantity.0.find(|chr: char| chr.is_alphabetic());
        let unit = start_of_unit.map_or("", |i| &quantity.0[i..]);
        let unit_factor = match unit {
            "Mi" => 1.0,
            "Gi" => 1024.0,
            _ => todo!(),
        };
        let amount = start_of_unit
            .map_or(quantity.0.as_ref(), |i| &quantity.0[..i])
            .parse::<f64>()
            .unwrap()
            * unit_factor;
        Self { mebibytes: amount }
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
