use crate::compat::{denied, head, home, line_denied, pystr, splitlines, tail, Result};
use crate::process;
use crate::Sprouts;
use num_bigint::BigInt;
use num_traits::ToPrimitive;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

fn selected_exe(variable: &str, name: &str, fallback: PathBuf) -> PathBuf {
    std::env::var_os(variable)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| process::which(name))
        .unwrap_or(fallback)
}

impl Sprouts {
    pub fn kiwi(&self, args: &[String]) -> Result<Value> {
        let exe = selected_exe("KIWI_BIN", "kiwi", home().join(".local/bin/kiwi"));
        let output = process::run(&exe, args, Some(120))?;
        if ![0, 9, 10, 11].contains(&output.code) {
            let code = output.code.to_string();
            let err = output
                .err
                .as_deref()
                .ok_or("'NoneType' object has no attribute 'strip'")?;
            return Err(format!(
                "kiwi: {}",
                head(
                    if err.trim().is_empty() {
                        &code
                    } else {
                        err.trim()
                    },
                    400
                )
            ));
        }
        crate::protocol::parse_json(
            output
                .out
                .as_deref()
                .ok_or("the JSON object must be str, bytes or bytearray, not NoneType")?,
        )
    }

    pub fn context_for(&mut self, args: &Value) -> Result<Value> {
        let mut bundle = self.context_bundle(args)?;
        if let Some(question) = args.get("question").filter(|value| !value.is_null()) {
            bundle["answer"] = self.ask("ask", &json!({
                "question": question,
                "options": args.get("options").unwrap_or(&Value::Null),
                "task": args["task"],
                "principal": args["principal"]
            })).unwrap_or_else(|error| json!({
                "status": "needs_user", "answer": "오너 확인 필요", "reason": "error", "error": error
            }));
        }
        Ok(bundle)
    }

    fn context_bundle(&mut self, args: &Value) -> Result<Value> {
        let default_budget = json!(4096);
        let default_scope = json!("*");
        let mut command = vec![
            "--principal".into(),
            pystr(&args["principal"]),
            "--task".into(),
            pystr(&args["task"]),
            "--budget".into(),
            pystr(args.get("budget").unwrap_or(&default_budget)),
            "--scope".into(),
            pystr(args.get("scope").unwrap_or(&default_scope)),
        ];
        for key in ["actions", "paths"] {
            if let Some(value) = args.get(key).filter(|value| crate::compat::truthy(value)) {
                let Some(values) = value.as_array() else {
                    return Err("can only join an iterable".into());
                };
                let mut strings = vec![];
                for (index, value) in values.iter().enumerate() {
                    strings.push(value.as_str().ok_or_else(|| {
                        format!(
                            "sequence item {index}: expected str instance, {} found",
                            crate::protocol::type_name(value)
                        )
                    })?);
                }
                command.extend([format!("--{key}"), strings.join(",")]);
            }
        }
        if let Some(task) = args
            .get("task_id")
            .filter(|value| crate::compat::truthy(value))
        {
            command.extend(["--task-id".into(), pystr(task)]);
        }
        if std::env::var("KIWI_DECISIONS").unwrap_or_else(|_| "on".into()) != "off" {
            if let Ok(value) = self
                .decisions
                .call(json!({"root":self.kiwi_root,"method":"brief","args":command}))
            {
                return Ok(value);
            }
            if self.decisions.fatal_code.is_some() {
                return Err(String::new());
            }
        }
        command.insert(0, "brief".into());
        self.kiwi(&command)
    }

    pub fn kiwi_rule(&self, id: &str) -> Result<Value> {
        if denied(Path::new(id)) {
            return Err("denied".into());
        }
        self.kiwi(&["rule".into(), id.into()])
    }

    pub fn ops_graph(&self, args: &[String]) -> Result<Value> {
        if args.first().is_none_or(|value| {
            !["rules", "trace", "owners", "preflight", "validate"].contains(&value.as_str())
        }) {
            return Err("ops_graph allows: rules, trace, owners, preflight, validate".into());
        }
        let exe = selected_exe(
            "KIWI_GRAPH_BIN",
            "kiwi-graph",
            PathBuf::from("/tmp/kiwi-graph-target/release/kiwi-graph"),
        );
        if !exe.exists() && process::which(&exe.to_string_lossy()).is_none() {
            return Err("kiwi-graph binary not built (bootstrap.sh)".into());
        }
        let output = process::run(&exe, args, Some(120))?;
        Ok(
            json!({"exit":output.code,"out":tail(output.out.as_deref().ok_or("'NoneType' object is not subscriptable")?,20000),"err":tail(output.err.as_deref().ok_or("'NoneType' object is not subscriptable")?,2000)}),
        )
    }

    pub fn docs_search(&self, query: &str, k: &Value) -> Result<Value> {
        for candidate in [
            std::env::var_os("KIWI_DOCS_SEARCH")
                .filter(|value| !value.is_empty())
                .map(PathBuf::from),
            Some(home().join(".kiwi/bin/kiwi-search")),
            Some(home().join(".kiwi/bin/brain-search")),
        ]
        .into_iter()
        .flatten()
        {
            if candidate.exists() {
                let output = process::run(
                    &candidate,
                    &[query.into(), "-k".into(), pystr(k)],
                    Some(300),
                )?;
                let filtered = splitlines(
                    output
                        .out
                        .as_deref()
                        .ok_or("'NoneType' object has no attribute 'splitlines'")?,
                )
                .into_iter()
                .filter(|line| !line_denied(line))
                .collect::<Vec<_>>()
                .join("\n");
                return Ok(
                    json!({"engine":"brain-search (EmbeddingGemma)","exit":output.code,"out":tail(&filtered,20000)}),
                );
            }
        }
        let Some(rg) = process::which("rg") else {
            return Ok(
                json!({"engine":"none (semantic index lives on Mac mini)","out":"rg not found; no local docs index on this box"}),
            );
        };
        let mut args: Vec<String> = [
            "-n",
            "-F",
            "-i",
            "-m",
            "3",
            "--hidden",
            "--no-ignore",
            "-g",
            "*.md",
            "-g",
            "*.toml",
            "-g",
            "*.txt",
            "-g",
            ".env*",
            "-g",
            "*.key",
            "-g",
            "*.pem",
            "-g",
            "!.git",
        ]
        .into_iter()
        .map(String::from)
        .collect();
        args.extend([
            "--".into(),
            query.into(),
            self.kiwi_root.to_string_lossy().into_owned(),
        ]);
        let output = process::run(&rg, &args, None)?;
        let mut keep = vec![];
        for line in splitlines(
            output
                .out
                .as_deref()
                .ok_or("'NoneType' object has no attribute 'splitlines'")?,
        ) {
            let path = line.split(':').next().unwrap_or("");
            let candidate = if line.starts_with(self.kiwi_root.to_string_lossy().as_ref()) {
                let file = crate::compat::resolved(Path::new(path))?;
                let root = crate::compat::resolved(&self.kiwi_root)?;
                file.strip_prefix(&root)
                    .map(Path::to_path_buf)
                    .map_err(|_| {
                        format!(
                            "'{}' is not in the subpath of '{}'",
                            file.display(),
                            root.display()
                        )
                    })?
            } else {
                PathBuf::from(path)
            };
            if !denied(&candidate) {
                keep.push(line);
            }
        }
        let count: BigInt = k
            .as_bool()
            .map(|value| BigInt::from(u8::from(value)))
            .or_else(|| {
                k.as_number()
                    .and_then(|number| number.to_string().parse().ok())
            })
            .ok_or("slice indices must be integers or None or have an __index__ method")?;
        let count = count * 3u8;
        let size = if count < BigInt::from(0) {
            keep.len()
                .saturating_sub((-count).to_usize().unwrap_or(usize::MAX))
        } else {
            count.to_usize().unwrap_or(usize::MAX)
        };
        Ok(
            json!({"engine":"rg (box fallback; semantic index lives on Mac mini)","out":keep.into_iter().take(size).collect::<Vec<_>>().join("\n")}),
        )
    }

    pub fn query(&mut self, q: &str, path: &str, subject: Option<&str>) -> Result<Value> {
        let code = self.code_find(q, path).unwrap_or_else(Value::String);
        let docs = self.docs_search(q, &json!(5))?;
        let ops = match subject.filter(|subject| !subject.is_empty()) {
            Some(subject) => self
                .ops_graph(&[
                    "preflight".into(),
                    "--repo".into(),
                    self.kiwi_root.to_string_lossy().into_owned(),
                    "--subject".into(),
                    subject.into(),
                ])
                .unwrap_or_else(Value::String),
            None => Value::Null,
        };
        let mut result = json!({"code":null,"docs":null,"ops":null});
        result["code"] = code;
        result["docs"] = docs;
        result["ops"] = ops;
        Ok(result)
    }

    pub fn decide(&mut self, args: &Value) -> Result<Value> {
        let mut request = json!({"root":self.kiwi_root,"method":"decide","args":null});
        request["args"] = Value::Array(vec![
            args["kind"].clone(),
            args["question"].clone(),
            args["input"].clone(),
            args.get("options").cloned().unwrap_or(Value::Null),
        ]);
        self.decisions.call(request)
    }

    pub fn ask(&mut self, method: &str, args: &Value) -> Result<Value> {
        crate::protocol::validate(method, args)?;
        self.decisions
            .call(json!({"root":self.kiwi_root,"method":method,"args":args}))
    }

    pub fn ask_stats(&mut self, args: &Value) -> Result<Value> {
        self.ask("ask_stats", args)
    }

    pub fn rule_resources(&self) -> Result<Value> {
        let db = std::env::var_os("KIWI_DB")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                std::env::var_os("KIWI_HOME")
                    .map(PathBuf::from)
                    .unwrap_or_else(|| home().join(".kiwi"))
                    .join("kiwi.db")
            });
        if !db.is_file() {
            return Ok(json!([]));
        }
        let rows = kiwi_core::rule_resources(&db)?;
        Ok(Value::Array(rows.into_iter().filter(|row| !denied(Path::new(&row.path))).map(|row| {
            let title = row.title.filter(|title| !title.is_empty()).unwrap_or_else(|| row.id.clone());
            json!({"uri":format!("kiwi://rule/{}",row.id),"name":row.id,"title":title,"description":head(&row.summary.unwrap_or_default(),200),"mimeType":"text/markdown"})
        }).collect()))
    }

    pub fn read_resource(&self, uri: &str) -> Result<Value> {
        let id = uri
            .strip_prefix("kiwi://rule/")
            .ok_or_else(|| format!("unknown resource {uri}"))?;
        let rule = self.kiwi_rule(id)?;
        let body = rule.get("body").ok_or("'body'")?;
        let mut content = json!({"uri":uri,"mimeType":"text/markdown","text":null});
        content["text"] = body.clone();
        let mut response = json!({"contents":[]});
        response["contents"] = Value::Array(vec![content]);
        Ok(response)
    }
}
