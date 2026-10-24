mod compiler;
mod context;
mod intent;
mod lint;
mod ops;
mod oss;
mod util;
mod writer;
use util::*;

const HELP: &str = "kiwi — Kiwi core
  intent add|stop|revoke|list   --principal user --message-ref REF --goal G --scope S [--kind instruction|reference] [--supersedes ID]
  pref add                       --statement S --evidence E [--uncertainty 0.5]   (never overrides intents)
  approve add|check|revoke       --subject --action --resource [--recipient --data-scope --cost-limit --expires-in SECS]
  task start|guard|finish        --task-id T [--scope S] [--fencing-token N] [--status done|failed|cancelled|rollback_complete]
  compile                        --src REPO [--out DB] [--sha SHA]
  brief|context_for              --principal BOT --task TEXT [--question Q --options a,b --actions a,b --paths p,q --budget 4096 --scores scores.json --scope S --task-id T]
  write|propose                  --repo CLONE --principal P --patch FILE --message M [--base SHA --proposal-id ID --approval-ref APR]
                                 states: accepted → committed → published → activated | rejected | conflicted | held (exit 11)
  refresh                        --repo CLONE   compile HEAD and activate atomically
  status                         active sha, epoch, lease, outbox, brief stats
  backup                         --to PATH      consistent ops.db copy
  oss export|scan|tick|flush|status  --repo CLONE [--out DIR | --dir DIR] [--sha SHA] [--dry-run]   sanitized public copy (tokens oss.*); scan fails closed
  compact                        --task T [--budget 4096 --keep-tail 2] < context.txt   keep relevant chunks (Decisions, fallback recency)
  index                          --src REPO [--check]   regenerate rules/README.md index block from rule frontmatter
  lint                           --src REPO [--changed p,q]   rule/board checks (exit 1 on errors); used by the pre-commit hook
  rule ID                        one node (kiwi://rule/ID)
exit: 0 ok, 9 stale/fenced/cancelled, 10 revise (re-read context_for), 2 error";

fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let a = Args::parse(&argv);
    let p = |i: usize| a.pos.get(i).map(|s| s.as_str()).unwrap_or("");
    let r: Res<(serde_json::Value, i32)> = match p(0) {
        "intent" => intent::intent(p(1), &a).map(|v| (v, 0)),
        "pref" => intent::pref(&a).map(|v| (v, 0)),
        "approve" => intent::approve(p(1), &a).map(|v| (v, 0)),
        "task" => intent::task(p(1), &a),
        "compile" => {
            let src = std::path::PathBuf::from(a.get("src").unwrap_or(".".into()));
            let out = a.get("out").map(std::path::PathBuf::from).unwrap_or_else(|| home().join("kiwi.db.next"));
            let sha = a.get("sha").unwrap_or_else(|| git_head(&src));
            compiler::compile(&src, &out, &sha).map(|v| (v, 0))
        }
        "brief" | "context_for" => {
            let scores = match a.get("scores") { Some(f) => std::fs::read_to_string(f).ok().and_then(|s| serde_json::from_str::<std::collections::HashMap<String, f64>>(&s).ok()).unwrap_or_default(), None => Default::default() };
            let paths = a.list("paths");
            let req = context::Req { principal: a.get("principal").unwrap_or("unknown".into()), task: a.get("task").unwrap_or_default(), actions: a.list("actions"), paths: paths.clone(),
                budget: a.get("budget").and_then(|s| s.parse().ok()).unwrap_or(4096), scores, tree_hash: a.get("tree-hash").unwrap_or_default(), dirty_hash: a.get("dirty-diff-hash").unwrap_or_default(),
                task_id: a.get("task-id").unwrap_or_default(), scope: a.get("scope").unwrap_or("*".into()), no_cache: a.flag("no-cache"),
                question: a.get("question"), options: a.list("options") };
            context::context_for(&req).map(|v| (v, 0))
        }
        "write" | "propose" => writer::write(&a),
        "refresh" => writer::refresh(&a).map(|v| (v, 0)),
        "status" => writer::status().map(|v| (v, 0)),
        "backup" => writer::backup(&a).map(|v| (v, 0)),
        "oss" => oss::oss(p(1), &a),
        "compact" => {
            let mut t = String::new(); use std::io::Read; std::io::stdin().read_to_string(&mut t).map_err(e).ok();
            context::compact(&a.get("task").unwrap_or_default(), &t, a.get("budget").and_then(|s| s.parse().ok()).unwrap_or(4096), a.get("keep-tail").and_then(|s| s.parse().ok()).unwrap_or(2)).map(|v| (v, 0))
        }
        "index" => lint::index(std::path::Path::new(&a.get("src").unwrap_or(".".into())), a.flag("check")),
        "lint" => lint::lint(std::path::Path::new(&a.get("src").unwrap_or(".".into())), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0), &a.list("changed")),
        "rule" => context::get_rule(p(1)).map(|v| (v, 0)),
        _ => { println!("{}", HELP); std::process::exit(if p(0).is_empty() || p(0) == "help" { 0 } else { 2 }) }
    };
    match r {
        Ok((v, code)) => { println!("{}", serde_json::to_string_pretty(&v).unwrap()); std::process::exit(code) }
        Err(m) => { eprintln!("kiwi: {}", m); std::process::exit(2) }
    }
}

pub fn git_head(dir: &std::path::Path) -> String {
    std::process::Command::new("git").arg("-C").arg(dir).args(["rev-parse", "HEAD"]).output().ok()
        .filter(|o| o.status.success()).map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).unwrap_or_else(|| "uncommitted".into())
}
