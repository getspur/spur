use std::collections::BTreeMap;

pub type Props = BTreeMap<&'static str, serde_json::Value>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier {
    One,
    Two,
}

pub trait Event {
    const NAME: &'static str;
    const TIER: Tier;

    fn into_props(self) -> Props;
}

pub(crate) mod sealed {
    pub trait Sealed {}
}

pub trait IntoProp: sealed::Sealed {
    fn into_prop(self) -> serde_json::Value;
}

impl sealed::Sealed for bool {}
impl IntoProp for bool {
    fn into_prop(self) -> serde_json::Value {
        self.into()
    }
}

impl sealed::Sealed for i32 {}
impl IntoProp for i32 {
    fn into_prop(self) -> serde_json::Value {
        self.into()
    }
}

impl sealed::Sealed for i64 {}
impl IntoProp for i64 {
    fn into_prop(self) -> serde_json::Value {
        self.into()
    }
}

impl sealed::Sealed for u32 {}
impl IntoProp for u32 {
    fn into_prop(self) -> serde_json::Value {
        self.into()
    }
}

impl sealed::Sealed for u64 {}
impl IntoProp for u64 {
    fn into_prop(self) -> serde_json::Value {
        self.into()
    }
}

impl sealed::Sealed for f64 {}
impl IntoProp for f64 {
    fn into_prop(self) -> serde_json::Value {
        self.into()
    }
}

impl sealed::Sealed for &'static str {}
impl IntoProp for &'static str {
    fn into_prop(self) -> serde_json::Value {
        self.into()
    }
}
