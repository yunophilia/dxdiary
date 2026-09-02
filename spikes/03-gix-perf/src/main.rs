//! Spike 0.3 — Is gix fast enough, and does its API cover what crowsnest needs?
//!
//! Two questions, and the API one matters more:
//!   API : can gix express merge-base fork-point diffing, working-tree status,
//!         and per-line blame? (DESIGN.md §5)
//!   Perf: budget is diff < 200ms, blame < 1s, status < 200ms.
//!
//! Usage: spike-03-gix-perf <repo-path> [base-ref] [file-to-blame]

use std::time::{Duration, Instant};

use gix::bstr::{BStr, ByteSlice};

/// Trunk names tried in order when no base ref is given (DESIGN.md §5).
const TRUNK_CANDIDATES: &[&str] = &["main", "master", "develop", "trunk"];

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let repo_path = args.next().unwrap_or_else(|| ".".into());
    let base_arg = args.next();
    let blame_arg = args.next();

    println!("repo: {repo_path}");
    let t = Instant::now();
    let mut repo = gix::open(&repo_path)?;
    report("open repository", t.elapsed(), None);

    // Recommended by gix docs for repeated commit lookups (merge-base, blame).
    repo.object_cache_size_if_unset(4 * 1024 * 1024);

    let head_id = repo.head_id()?;
    println!("HEAD: {}", short(&head_id.to_string()));

    // --- fork-point resolution (DESIGN.md §5) ---------------------------
    let base_ref = match &base_arg {
        Some(r) => r.clone(),
        None => detect_trunk(&repo).ok_or("no trunk ref found; pass one explicitly")?,
    };
    let base_id = repo.rev_parse_single(base_ref.as_str())?;
    println!("base: {base_ref} @ {}", short(&base_id.to_string()));

    let t = Instant::now();
    let merge_base = repo.merge_base(base_id, head_id)?;
    let d_mb = t.elapsed();
    report("merge_base", d_mb, Some(Duration::from_millis(200)));
    println!("      fork point = {}", short(&merge_base.to_string()));

    // --- three-dot diff: merge_base..HEAD -------------------------------
    let mb_tree = repo.find_object(merge_base)?.peel_to_tree()?;
    let head_tree = repo.find_object(head_id)?.peel_to_tree()?;

    let t = Instant::now();
    let changes = repo.diff_tree_to_tree(Some(&mb_tree), Some(&head_tree), None)?;
    let d_diff = t.elapsed();
    report("diff merge_base..HEAD", d_diff, Some(Duration::from_millis(200)));
    println!("      {} changed path(s)", changes.len());
    for c in changes.iter().take(5) {
        println!("        {}", c.location());
    }
    if changes.len() > 5 {
        println!("        ... and {} more", changes.len() - 5);
    }

    // --- working tree status --------------------------------------------
    let t = Instant::now();
    let iter = repo
        .status(gix::progress::Discard)?
        .into_index_worktree_iter(Vec::new())?;
    let mut n_status = 0usize;
    let mut samples = Vec::new();
    for item in iter {
        let item = item?;
        if samples.len() < 5 {
            samples.push(item.rela_path().to_str_lossy().into_owned());
        }
        n_status += 1;
    }
    let d_status = t.elapsed();
    report("status (index↔worktree)", d_status, Some(Duration::from_millis(200)));
    println!("      {n_status} dirty path(s)");
    for s in &samples {
        println!("        {s}");
    }

    // --- blame ------------------------------------------------------------
    let blame_path = blame_arg.or_else(|| {
        changes
            .first()
            .map(|c| c.location().to_str_lossy().into_owned())
    });

    match blame_path {
        None => println!("\nblame: skipped (no file to blame)"),
        Some(path) => {
            println!("\nblame target: {path}");
            let t = Instant::now();
            let mut resource_cache = repo.diff_resource_cache_for_tree_diff()?;
            let outcome = gix::blame::file(
                &repo.objects,
                head_id.detach(),
                None,
                &mut resource_cache,
                BStr::new(path.as_bytes()),
                gix::blame::Options::default(),
            )?;
            let d_blame = t.elapsed();
            report("blame", d_blame, Some(Duration::from_secs(1)));

            let entries = outcome.entries;
            let lines: u32 = entries.iter().map(|e| e.len.get()).sum();
            println!("      {} hunk(s) covering {} line(s)", entries.len(), lines);
            for e in entries.iter().take(5) {
                println!(
                    "        L{:<5} +{:<4} {}",
                    e.start_in_blamed_file + 1,
                    e.len.get(),
                    short(&e.commit_id.to_string())
                );
            }
        }
    }

    Ok(())
}

fn detect_trunk(repo: &gix::Repository) -> Option<String> {
    for name in TRUNK_CANDIDATES {
        if repo.rev_parse_single(*name).is_ok() {
            return Some((*name).to_string());
        }
    }
    None
}

fn report(label: &str, elapsed: Duration, budget: Option<Duration>) {
    let ms = elapsed.as_secs_f64() * 1000.0;
    match budget {
        Some(b) => {
            let mark = if elapsed <= b { "OK  " } else { "OVER" };
            println!("[{mark}] {label:<26} {ms:>9.2} ms   (budget {:.0} ms)", b.as_secs_f64() * 1000.0);
        }
        None => println!("[    ] {label:<26} {ms:>9.2} ms"),
    }
}

fn short(oid: &str) -> String {
    oid.chars().take(10).collect()
}
