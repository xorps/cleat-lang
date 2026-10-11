//! Reading YAML documents (CRDs, input files) into JSON values.

use yaml_rust2::{Yaml, YamlLoader};

use cleat_core::Value;

/// All documents of a YAML stream.
pub fn parse_docs(src: &str) -> Result<Vec<Value>, String> {
    let docs = YamlLoader::load_from_str(src).map_err(|e| format!("invalid YAML: {e}"))?;
    docs.iter().map(to_value).collect()
}

fn to_value(y: &Yaml) -> Result<Value, String> {
    Ok(match y {
        Yaml::Null => Value::Null,
        Yaml::Boolean(b) => Value::Bool(*b),
        Yaml::Integer(i) => Value::Int(*i),
        Yaml::Real(s) => match parse_real(s) {
            Some(x) if x.is_finite() => Value::Float(x),
            // Values are JSON, which has no infinity or NaN.
            Some(_) => {
                return Err(format!(
                    "YAML number `{s}` is not a JSON number (infinity, NaN or out of range)"
                ));
            }
            None => return Err(format!("invalid YAML number `{s}`")),
        },
        Yaml::String(s) => Value::Str(s.clone()),
        Yaml::Array(xs) => Value::List(xs.iter().map(to_value).collect::<Result<_, _>>()?),
        Yaml::Hash(h) => {
            let mut es = Vec::with_capacity(h.len());
            for (k, v) in h {
                let key = match k {
                    Yaml::String(s) | Yaml::Real(s) => s.clone(),
                    Yaml::Integer(i) => i.to_string(),
                    Yaml::Boolean(b) => b.to_string(),
                    Yaml::Null => "null".to_string(),
                    _ => return Err("YAML mapping keys must be scalars".into()),
                };
                es.push((key, to_value(v)?));
            }
            Value::Obj(es)
        }
        Yaml::Alias(_) => return Err("YAML aliases are not supported".into()),
        Yaml::BadValue => return Err("invalid YAML value".into()),
    })
}

fn parse_real(s: &str) -> Option<f64> {
    match s.to_ascii_lowercase().as_str() {
        ".inf" | "+.inf" => Some(f64::INFINITY),
        "-.inf" => Some(f64::NEG_INFINITY),
        ".nan" => Some(f64::NAN),
        other => other.replace('_', "").parse().ok(),
    }
}
