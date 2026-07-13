use protobuf::MessageField;
use scip::symbol::format_symbol;
use scip::types::descriptor::Suffix;
use scip::types::{Descriptor, Package, Symbol};

#[derive(Clone, Debug)]
pub struct PackageInfo {
    pub name: String,
    pub version: String,
}

pub fn descriptor(name: &str, suffix: Suffix) -> Descriptor {
    Descriptor {
        name: name.to_string(),
        suffix: suffix.into(),
        ..Default::default()
    }
}

/// Format a global symbol string following the SCIP symbol grammar, e.g.
/// `scip-python python mypackage 1.0 `foo.bar`/Class#method().`
pub fn format_global(package: &PackageInfo, descriptors: Vec<Descriptor>) -> String {
    format_symbol(Symbol {
        scheme: "scip-python".to_string(),
        package: MessageField::some(Package {
            manager: "python".to_string(),
            name: package.name.clone(),
            version: package.version.clone(),
            ..Default::default()
        }),
        descriptors,
        ..Default::default()
    })
}

pub fn local_symbol(id: usize) -> String {
    format!("local {id}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_module_symbol() {
        let package = PackageInfo {
            name: "mypkg".to_string(),
            version: "1.0".to_string(),
        };
        let symbol = format_global(
            &package,
            vec![
                descriptor("foo.bar", Suffix::Namespace),
                descriptor("Baz", Suffix::Type),
                descriptor("method", Suffix::Method),
            ],
        );
        assert_eq!(
            symbol,
            "scip-python python mypkg 1.0 `foo.bar`/Baz#method()."
        );
    }
}
