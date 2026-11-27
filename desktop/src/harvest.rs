//! Infer HUD digit templates from score crops and the sequence of jump increments.
use crate::{Frame, Result};

use std::fs;
use std::io::{ErrorKind, Write};
use std::path::Path;
struct Glyph {
    feature: Vec<f32>,
    norm: f32,
    png: Vec<u8>,
}

impl Glyph {
    fn similar(&self, other: &Self) -> bool {
        let dot: f32 = self
            .feature
            .iter()
            .zip(&other.feature)
            .map(|(a, b)| a * b)
            .sum();
        dot / (self.norm * other.norm + 1e-6) > 0.95
    }
}

fn glyphs(frame: &Frame) -> Result<Vec<Glyph>> {
    crate::screen::segment_glyphs(frame, [0, 0, frame.width, frame.height])
        .map_err(|e| e.to_string())?
        .into_iter()
        .map(|raw| {
            let feature = crate::screen::glyph_feature(&raw).map_err(|e| e.to_string())?;
            let norm = feature.iter().map(|v| v * v).sum::<f32>().sqrt();
            Ok(Glyph {
                feature,
                norm,
                png: crate::screen::glyph_png(&raw)?,
            })
        })
        .collect()
}

struct Sample {
    jump: u128,
    predicted: u128,
    ids: Vec<usize>,
}

fn sample_name(name: &str) -> Option<(u128, u128)> {
    // Python re.search permits a prefix before the final jNNN_sXXXX.png.
    let (jump, score) = name
        .strip_suffix(".png")?
        .rsplit_once('j')?
        .1
        .split_once("_s")?;
    if jump.is_empty()
        || score.is_empty()
        || !jump
            .bytes()
            .chain(score.bytes())
            .all(|b| b.is_ascii_digit())
    {
        return None;
    }
    Some((jump.parse().ok()?, score.parse().ok()?))
}

fn evaluate(segment: &[Sample], base: u128, clusters: usize) -> (usize, Vec<(usize, u8)>) {
    let mut known = vec![None; clusters];
    let mut order = Vec::new();
    let mut conflicts = 0;
    for sample in segment {
        let delta = sample.predicted - segment[0].predicted;
        let Some(score) = base.checked_add(delta) else {
            conflicts += 3;
            continue;
        };
        let digits = score.to_string();
        if digits.len() != sample.ids.len() {
            conflicts += 3;
            continue;
        }
        for (&id, digit) in sample.ids.iter().zip(digits.bytes().map(|b| b - b'0')) {
            match known[id] {
                Some(old) => conflicts += usize::from(old != digit),
                None => {
                    known[id] = Some(digit);
                    order.push((id, digit));
                }
            }
        }
    }
    (conflicts, order)
}

fn labels(samples: &[Sample], clusters: usize) -> Vec<(usize, usize)> {
    let mut votes = vec![[0usize; 10]; clusters];
    let mut order = Vec::new();
    for segment in samples.chunk_by(|a, b| b.predicted > a.predicted && b.jump > a.jump) {
        // The first base with the fewest conflicts wins, including ties.
        let (base, (conflicts, mapping)) = (0..300)
            .map(|base| (base, evaluate(segment, base, clusters)))
            .min_by_key(|(_, (conflicts, _))| *conflicts)
            .unwrap();
        println!(
            "样本段（{} 条）：起始分 {base}，冲突 {conflicts}",
            segment.len()
        );
        for (id, digit) in mapping {
            if votes[id] == [0; 10] {
                order.push(id);
            }
            votes[id][digit as usize] += 1;
        }
    }

    let mut taken = [false; 10];
    let mut chosen = Vec::new();
    for id in order {
        let mut digits = (0..10).filter(|&d| votes[id][d] > 0);
        let digit = digits.next().unwrap();
        if digits.next().is_some() {
            println!("字形簇 {id} 标签不一致 {:?}，跳过", votes[id]);
        } else if !taken[digit] {
            chosen.push((id, digit));
            taken[digit] = true;
        }
    }
    chosen
}

fn write_template(path: &Path, png: &[u8]) -> Result<()> {
    // A reader can open a template while harvesting. Replace it only after
    // the entire PNG has been written in the destination directory.
    let temp = path.with_extension(format!("{}.tmp", std::process::id()));
    let mut file = fs::File::create_new(&temp).map_err(|e| format!("{}: {e}", temp.display()))?;
    let result = file.write_all(png);
    drop(file);
    let result = result.and_then(|()| fs::rename(&temp, path));
    if let Err(e) = result {
        let _ = fs::remove_file(&temp);
        return Err(format!("{}: {e}", path.display()));
    }
    Ok(())
}

pub(crate) fn run(score_dir: &Path, templates: &Path) -> Result<()> {
    let no_files = || {
        format!(
            "没有找到分数样本：{}（先跑一次 --save-debug 采集）",
            score_dir.display()
        )
    };
    let entries = fs::read_dir(score_dir).map_err(|e| {
        if e.kind() == ErrorKind::NotFound {
            no_files()
        } else {
            format!("{}: {e}", score_dir.display())
        }
    })?;
    let mut paths = Vec::new();
    for entry in entries {
        let path = entry.map_err(|e| e.to_string())?.path();
        if path
            .extension()
            .is_some_and(|s| s.eq_ignore_ascii_case("png"))
        {
            paths.push(path);
        }
    }
    if paths.is_empty() {
        return Err(no_files());
    }
    paths.sort_by_cached_key(|p| p.file_name().unwrap().to_string_lossy().to_lowercase());

    let mut samples = Vec::new();
    let mut clusters: Vec<(Glyph, usize)> = Vec::new();
    for path in paths {
        let Some((jump, predicted)) = path
            .file_name()
            .and_then(|s| s.to_str())
            .and_then(sample_name)
        else {
            continue;
        };
        let Ok(frame) = Frame::read(&path) else {
            continue;
        };
        let mut ids = Vec::new();
        for glyph in glyphs(&frame)? {
            // First matching cluster, keeping its original example for export.
            let id = if let Some(i) = clusters.iter().position(|(first, _)| glyph.similar(first)) {
                clusters[i].1 += 1;
                i
            } else {
                clusters.push((glyph, 1));
                clusters.len() - 1
            };
            ids.push(id);
        }
        samples.push(Sample {
            jump,
            predicted,
            ids,
        });
    }
    if samples.is_empty() {
        return Err("没有可读取的 jNNN_sXXXX.png 分数样本".into());
    }
    let chosen = labels(&samples, clusters.len());
    fs::create_dir_all(templates).map_err(|e| format!("{}: {e}", templates.display()))?;
    for &(id, digit) in &chosen {
        write_template(
            &templates.join(format!("digit_{digit}.png")),
            &clusters[id].0.png,
        )?;
        println!("数字模板 {digit}：样本 {} 个", clusters[id].1);
    }
    let missing: String = (0..10)
        .filter(|d| !chosen.iter().any(|(_, v)| v == d))
        .map(|d| char::from(b'0' + d as u8))
        .collect();
    let suffix = if missing.is_empty() {
        String::new()
    } else {
        format!("，缺少 {missing}（可再跑几局采集）")
    };
    println!(
        "完成：使用 {} 张样本，已生成 {} 个数字模板{suffix}",
        samples.len(),
        chosen.len()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn score_offsets_follow_digit_lengths_and_disagreements_are_skipped() {
        // Both lives cross 9 -> 10. Their predicted starts differ. Cluster 1
        // is labelled 9 in the first life and 8 in the second.
        let sample = |jump, predicted, ids: &[usize]| Sample {
            jump,
            predicted,
            ids: ids.to_vec(),
        };
        let samples = [
            sample(1, 120, &[0]),
            sample(2, 121, &[1]),
            sample(3, 122, &[2, 3]),
            sample(1, 210, &[1]),
            sample(2, 211, &[4]),
            sample(3, 212, &[2, 3]),
        ];
        assert_eq!(labels(&samples, 5), [(0, 8), (2, 1), (3, 0), (4, 9)]);
    }

    #[test]
    fn first_tied_base_and_first_voted_example_win() {
        let samples = [
            Sample {
                jump: 4,
                predicted: 40,
                ids: vec![1],
            },
            Sample {
                jump: 5,
                predicted: 39,
                ids: vec![0],
            },
        ];
        assert_eq!(labels(&samples, 2), [(1, 0)]);
        assert_eq!(sample_name("round-j001_s0002.png"), Some((1, 2)));
        assert_eq!(sample_name("j001_s0002-other.png"), None);
    }
}
