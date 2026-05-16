use serde_json::Value;

pub fn flatten_dot_args(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut flat: serde_json::Map<String, Value> = serde_json::Map::new();
            for (k, v) in map {
                if let Some(dot_pos) = k.find('.') {
                    let (root, rest) = k.split_at(dot_pos);
                    let sub_key = &rest[1..];
                    flat.entry(root.to_string())
                        .or_insert_with(|| Value::Object(Default::default()))
                        .as_object_mut()
                        .unwrap()
                        .insert(sub_key.to_string(), v.clone());
                } else {
                    flat.insert(k.clone(), v.clone());
                }
            }
            Value::Object(flat)
        }
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn flattens_dot_notation() {
        let input = json!({"file.write": {"path": "/tmp/x", "content": "hello"}});
        let output = flatten_dot_args(&input);
        let file = output.get("file").unwrap();
        let write = file.get("write").unwrap();
        assert_eq!(write.get("path").unwrap().as_str().unwrap(), "/tmp/x");
    }

    #[test]
    fn preserves_normal_keys() {
        let input = json!({"name": "bash", "args": {"cmd": "ls"}});
        let output = flatten_dot_args(&input);
        assert_eq!(output.get("name").unwrap().as_str().unwrap(), "bash");
    }

    #[test]
    fn no_dots_no_change() {
        let input = json!({"key": "value"});
        assert_eq!(flatten_dot_args(&input), input);
    }

    #[test]
    fn non_object_passthrough() {
        assert_eq!(flatten_dot_args(&Value::String("hi".into())), Value::String("hi".into()));
        assert_eq!(flatten_dot_args(&Value::Number(42.into())), Value::Number(42.into()));
    }
}
