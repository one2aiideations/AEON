use mouli_lab::aetheron_ml::{gradcheck_model, AetheronMlCfg, AetheronMlLearner};
use mouli_lab::baselines::{gradcheck_mlp, MlpLearner};
use mouli_lab::continual::{run_split, RunCfg, Strategy};
use mouli_lab::core::{EvalNorm, Learner, Mode};
use mouli_lab::data::{parse_idx, synthetic_blobs};
use mouli_lab::rng::Rng;
use mouli_lab::temporal::{make_batch, GatedAetheron, MiniTransformer, RecurrentAetheron};

fn assert_all(checks: &[mouli_lab::core::GradCheck]) {
    for c in checks {
        assert!(
            c.pass,
            "{}: max_abs={:e} max_rel={:e}",
            c.name, c.max_abs, c.max_rel
        );
    }
    assert!(!checks.is_empty());
}

#[test]
fn mlp_gradients_match_finite_differences() {
    let ds = synthetic_blobs(16, 6, 3, 1);
    let m = MlpLearner::new(6, 5, 3, 2, 0.05);
    let mut rng = Rng::new(3);
    assert_all(&gradcheck_mlp(&m, &ds.x, &ds.y, 12, &mut rng));
}

#[test]
fn aetheron_gradients_match_finite_differences() {
    let ds = synthetic_blobs(12, 6, 3, 4);
    let cfg = AetheronMlCfg {
        in_dim: 6,
        units: 4,
        branches: 2,
        hidden: 3,
        n_classes: 3,
        dk: 3,
        eps: -1.0,
        ..Default::default()
    };
    let mut m = AetheronMlLearner::new(cfg, 5);
    // Give the EMA path non-trivial statistics so the fixed-EMA check is not degenerate.
    for _ in 0..3 {
        m.step(&ds.x, &ds.y, Mode::Encode);
    }
    let mut rng = Rng::new(6);
    assert_all(&gradcheck_model(&m, &ds.x, &ds.y, 10, &mut rng));
}

#[test]
fn temporal_gradients_match_finite_differences() {
    let mut rng = Rng::new(7);
    let (xs, y) = make_batch(5, 6, 3, 0.1, &mut rng);
    assert_all(&RecurrentAetheron::new(5, 3, 2, 3, 8).gradcheck(&xs, &y, 8, &mut rng));
    assert_all(&GatedAetheron::new(5, 3, 2, 3, 9).gradcheck(&xs, &y, 8, &mut rng));
    assert_all(&MiniTransformer::new(5, 4, 10).gradcheck(&xs, &y, 8, &mut rng));
}

#[test]
fn rng_is_deterministic_and_in_range() {
    let mut a = Rng::new(42);
    let mut b = Rng::new(42);
    for _ in 0..100 {
        assert_eq!(a.next_u64(), b.next_u64());
    }
    let mut r = Rng::new(1);
    for _ in 0..1000 {
        let u = r.uniform();
        assert!((0.0..1.0).contains(&u));
        assert!(r.below(7) < 7);
    }
    assert_eq!(r.below(0), 0);
    assert_eq!(r.categorical(&[]), 0);
    assert_eq!(r.categorical(&[0.0, 0.0]), 0);
    assert_eq!(r.categorical(&[0.0, 1.0]), 1);
}

fn idx(n: u32, rows: u32, cols: u32, fill: u8) -> (Vec<u8>, Vec<u8>) {
    let mut img = Vec::new();
    for v in [2051u32, n, rows, cols] {
        img.extend_from_slice(&v.to_be_bytes());
    }
    img.extend(std::iter::repeat_n(fill, (n * rows * cols) as usize));
    let mut lab = Vec::new();
    for v in [2049u32, n] {
        lab.extend_from_slice(&v.to_be_bytes());
    }
    lab.extend((0..n).map(|i| (i % 10) as u8));
    (img, lab)
}

#[test]
fn idx_parser_accepts_valid_and_rejects_damaged_input() {
    let (img, lab) = idx(3, 2, 2, 9);
    let ds = parse_idx(&img, &lab, "i", "l").unwrap();
    assert_eq!((ds.x.r, ds.x.c, ds.y.len()), (3, 4, 3));
    assert!(parse_idx(&img[..img.len() - 1], &lab, "i", "l").is_err());
    assert!(parse_idx(&img, &lab[..lab.len() - 1], "i", "l").is_err());
    assert!(parse_idx(&img[..10], &lab, "i", "l").is_err());
    let mut bad = img.clone();
    bad[3] = 0;
    assert!(parse_idx(&bad, &lab, "i", "l").is_err());
    let mut huge = img.clone();
    huge[4..8].copy_from_slice(&u32::MAX.to_be_bytes());
    huge[8..12].copy_from_slice(&u32::MAX.to_be_bytes());
    assert!(parse_idx(&huge, &lab, "i", "l").is_err());
}

#[test]
fn mlp_learns_blobs() {
    let ds = synthetic_blobs(300, 8, 3, 11);
    let mut m = MlpLearner::new(8, 16, 10, 12, 0.05);
    for _ in 0..30 {
        for chunk in (0..300).collect::<Vec<_>>().chunks(30) {
            let s = ds.subset(chunk);
            m.step(&s.x, &s.y, Mode::Encode);
        }
    }
    let lg = m.logits(&ds.x, EvalNorm::FixedEma);
    let ok = (0..300)
        .filter(|&i| {
            (0..10)
                .max_by(|&a, &b| lg.d[i * 10 + a].total_cmp(&lg.d[i * 10 + b]))
                .unwrap() as u8
                == ds.y[i]
        })
        .count();
    assert!(ok > 270, "accuracy {}/300", ok);
}

#[test]
fn recurrent_model_learns_delayed_match() {
    let mut rng = Rng::new(21);
    let mut m = GatedAetheron::new(6, 8, 2, 6, 22);
    let mut last = f64::MAX;
    for i in 0..400 {
        let (xs, y) = make_batch(6, 32, 4, 0.05, &mut rng);
        let l = m.step(&xs, &y, 0.01);
        if i >= 380 {
            last = last.min(l);
        }
    }
    assert!(last < 0.69, "loss never fell below chance: {}", last);
}

#[test]
fn split_run_is_reproducible_and_honestly_labelled() {
    let mut tr = synthetic_blobs(500, 8, 10, 31);
    let te = synthetic_blobs(200, 8, 10, 32);
    tr.y.iter_mut().for_each(|y| *y %= 10);
    let cfg = RunCfg {
        epochs: 1,
        batch: 32,
        lr: 0.05,
        ewc_lambda: 100.0,
        mem_per_class: 5,
    };
    let a = run_split("mlp", Strategy::None, 1, &tr, &te, &cfg);
    let b = run_split("mlp", Strategy::None, 1, &tr, &te, &cfg);
    assert_eq!(a.final_avg, b.final_avg);
    let j = run_split("aetheron", Strategy::Joint, 1, &tr, &te, &cfg);
    assert_eq!(j.arch, "mlp", "Joint is MLP-only and must say so");
    assert_eq!(Strategy::parse("ewc"), Some(Strategy::Ewc));
    assert_eq!(Strategy::parse("nope"), None);
}

fn ablation_cfg() -> AetheronMlCfg {
    AetheronMlCfg {
        in_dim: 6,
        units: 4,
        branches: 2,
        hidden: 3,
        n_classes: 3,
        dk: 3,
        eps: -1.0,
        ..Default::default()
    }
}

#[test]
fn the_attention_switch_changes_the_output_and_its_gradients_are_still_correct() {
    let ds = synthetic_blobs(12, 6, 3, 4);
    let mut on = AetheronMlLearner::new(ablation_cfg(), 5);
    let off_cfg = AetheronMlCfg {
        attention: false,
        ..ablation_cfg()
    };
    let mut off = AetheronMlLearner::new(off_cfg, 5);
    // Same seed and weights, different forward pass.
    let a = on.logits(&ds.x, EvalNorm::FixedEma);
    let b = off.logits(&ds.x, EvalNorm::FixedEma);
    assert!(
        a.d.iter().zip(&b.d).any(|(x, y)| (x - y).abs() > 1e-9),
        "attention must matter when on"
    );
    // Gradients with attention off still match finite differences.
    for _ in 0..3 {
        off.step(&ds.x, &ds.y, Mode::Encode);
    }
    let mut rng = Rng::new(6);
    assert_all(&gradcheck_model(&off, &ds.x, &ds.y, 10, &mut rng));
}

#[test]
fn with_attention_off_the_query_and_key_weights_do_not_move_but_with_it_on_they_do() {
    let ds = synthetic_blobs(24, 6, 3, 4);
    for attention in [false, true] {
        let cfg = AetheronMlCfg {
            attention,
            ..ablation_cfg()
        };
        let mut m = AetheronMlLearner::new(cfg, 5);
        let (wq, wk) = (m.unit.wq.d.clone(), m.unit.wk.d.clone());
        let before = m.logits(&ds.x, EvalNorm::FixedEma);
        for _ in 0..10 {
            m.step(&ds.x, &ds.y, Mode::Encode);
        }
        let after = m.logits(&ds.x, EvalNorm::FixedEma);
        assert!(
            before
                .d
                .iter()
                .zip(&after.d)
                .any(|(x, y)| (x - y).abs() > 1e-9),
            "it still learns"
        );
        let moved = m.unit.wq.d != wq || m.unit.wk.d != wk;
        assert_eq!(
            moved, attention,
            "attention={attention}: query/key weights moved = {moved}"
        );
    }
}

#[test]
fn ablation_architectures_run_and_keep_their_own_names() {
    let tr = synthetic_blobs(300, 8, 10, 41);
    let te = synthetic_blobs(100, 8, 10, 42);
    let cfg = RunCfg {
        epochs: 1,
        batch: 32,
        lr: 0.05,
        ewc_lambda: 100.0,
        mem_per_class: 5,
    };
    for arch in [
        "aetheron-no-attention",
        "aetheron-no-gate",
        "aetheron-no-homeostasis",
    ] {
        let r = run_split(arch, Strategy::None, 1, &tr, &te, &cfg);
        assert_eq!(r.arch, arch);
        assert!(r.final_avg.is_finite());
    }
    // An unknown name does not silently become Aetheron.
    assert_eq!(
        run_split("aetheron-no-everything", Strategy::None, 1, &tr, &te, &cfg).arch,
        "mlp"
    );
}
