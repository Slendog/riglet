//! `--param NAME=VALUE` parsing. NAME matches a parameter's internal id,
//! its name, or one of its export ids (Live2D string, Inochi2D number).
//! A prefix (`id:`, `name:`, `live2d:`, `inochi2d:`) restricts the match to
//! that kind, which resolves any ambiguity. Ambiguous matches are an error
//! that lists the candidates; nothing is guessed. Values must be finite.

use riglet_format::model::ParamKind;
use riglet_format::{Document, Rig};

/// Longest accepted `--param` argument; arguments are untrusted.
const MAX_ARG_LEN: usize = 1024;

/// Returns the parameter index and its `[component 0, component 1]` value.
pub fn parse(arg: &str, doc: &Document, rig: &Rig) -> Result<(usize, [f32; 2]), String> {
    if arg.len() > MAX_ARG_LEN {
        return Err(format!("--param argument is longer than {MAX_ARG_LEN} bytes"));
    }
    let (name, value) = arg.split_once('=').ok_or_else(|| format!("--param `{arg}` must be NAME=VALUE"))?;
    let name = name.trim();
    let index = resolve(name, doc, rig)?;
    let kind = rig.params.get(index).map_or(ParamKind::OneD, |p| p.kind);
    let parts: Vec<&str> = value.split(',').map(str::trim).collect();
    let numbers = parts
        .iter()
        .map(|t| t.parse::<f32>().map_err(|_| format!("--param {name}: `{t}` is not a number")))
        .collect::<Result<Vec<f32>, String>>()?;
    if let Some(bad) = numbers.iter().find(|v| !v.is_finite()) {
        return Err(format!("--param {name}: value {bad} is not finite"));
    }
    match (kind, numbers.as_slice()) {
        (ParamKind::OneD, [x]) => Ok((index, [*x, 0.0])),
        (ParamKind::TwoD, [x, y]) => Ok((index, [*x, *y])),
        (ParamKind::OneD, _) => Err(format!("--param {name}: 1d parameter takes one value")),
        (ParamKind::TwoD, _) => Err(format!("--param {name}: 2d parameter takes two values, X,Y")),
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Id,
    Name,
    Live2d,
    Inochi2d,
}

impl Kind {
    fn label(self) -> &'static str {
        match self {
            Kind::Id => "id",
            Kind::Name => "name",
            Kind::Live2d => "live2d",
            Kind::Inochi2d => "inochi2d",
        }
    }
}

/// Finds the one parameter `name` refers to.
fn resolve(name: &str, doc: &Document, rig: &Rig) -> Result<usize, String> {
    let all = [Kind::Id, Kind::Name, Kind::Live2d, Kind::Inochi2d];
    let (kinds, name): (&[Kind], &str) = match name.split_once(':') {
        Some((prefix, rest)) => match all.iter().position(|k| k.label() == prefix) {
            Some(i) => (all.get(i..=i).unwrap_or(&[]), rest),
            None => (&all, name),
        },
        None => (&all, name),
    };
    let model = &doc.model;
    let mut matches: Vec<(&str, &str, &'static str)> = Vec::new();
    for p in &model.parameters {
        let live2d = model.export_ids.live2d.as_ref().and_then(|m| m.get(&p.id));
        let inochi2d = model.export_ids.inochi2d.as_ref().and_then(|m| m.get(&p.id));
        let hit = |k: Kind| match k {
            Kind::Id => p.id == name,
            Kind::Name => p.name == name,
            Kind::Live2d => live2d.is_some_and(|l| l == name),
            Kind::Inochi2d => inochi2d.is_some_and(|i| i.to_string() == name),
        };
        let how = kinds.iter().copied().find(|&k| hit(k)).map(Kind::label);
        if let Some(how) = how {
            matches.push((&p.id, &p.name, how));
        }
    }
    match matches.as_slice() {
        [] => Err(format!("no parameter matches `{name}` (by id, name or export id); see `riglet info`")),
        [(id, _, _)] => rig.param_index(id).ok_or_else(|| format!("parameter `{id}` is not usable")),
        many => {
            let list: Vec<String> =
                many.iter().map(|(id, n, how)| format!("  {n} (id {id}, matched by {how})")).collect();
            Err(format!(
                "`{name}` is ambiguous, it matches {} parameters:\n{}\nselect one with a prefix, e.g. `id:{}`",
                many.len(),
                list.join("\n"),
                many.first().map_or("", |m| m.0)
            ))
        }
    }
}
