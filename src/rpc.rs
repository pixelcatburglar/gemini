use crate::{
    pickle::Value as V,
    state::{Result, State},
};

impl State {
    /// Preserve Python's positional/default/keyword binding, including methods
    /// with *rest or **kw. Metadata contains signatures only, never Python code.
    pub fn arguments(
        &self,
        kind: &str,
        target: &str,
        method: &str,
        args: V,
        keywords: V,
    ) -> Result {
        // New native handlers must also work with an existing database whose
        // original RPC catalog predates them. No volume reset/reseed is needed.
        let native = if kind == "service" {
            crate::station::signature(target, method)
        } else {
            V::None
        };
        let spec = if !native.is_none() {
            native
        } else if kind == "service" {
            self.data["rpc.services"][target][method].or(self.data["rpc.any"][method].clone())
        } else {
            self.data["rpc.objects"][target][method].clone()
        };
        if spec.is_none() {
            return Err(format!("{target}.{method} is not implemented by this POC server").into());
        }
        if !matches!(args, V::Tuple(_) | V::List(_)) {
            return Err("RPC arguments must be a tuple or list".into());
        }
        let mut values = args.seq().to_vec();
        let params = spec["params"].seq();
        let positional = values.len();
        if positional > params.len() && !spec["varargs"].truth() {
            return Err("too many RPC arguments".into());
        }
        for (key, _) in keywords.pairs() {
            if !params.iter().any(|p| p[0] == *key) && !spec["keywords"].truth() {
                return Err(format!("unexpected keyword {}", key.text()).into());
            }
        }
        for (i, p) in params.iter().enumerate() {
            let keyword = keywords
                .pairs()
                .iter()
                .find(|(k, _)| *k == p[0])
                .map(|(_, v)| v.clone());
            if i < positional {
                if keyword.is_some() {
                    return Err("duplicate RPC argument".into());
                }
                continue;
            }
            if let Some(v) = keyword {
                values.push(v);
            } else if p[1].truth() {
                values.push(p[2].clone());
            } else {
                return Err(format!("missing RPC argument {}", p[0].text()).into());
            }
        }
        Ok(V::Tuple(values))
    }
}
