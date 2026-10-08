//! Small recorded graphs make individual attachments and lane changes inspectable.

use std::{collections::BTreeMap, path::Path};

use super::{
    Action, Binding, Engine, Position, RepositoryChainBinding, Result, View, activity, read,
};

type Step = (u64, u64, Vec<u64>, &'static str);
type Case = (&'static str, Vec<Step>);

pub(super) fn export(root: &Path) -> Result {
    let mut windows = BTreeMap::new();
    for (name, steps) in cases() {
        let chain = root.join("simple").join(name);
        let engine = Engine::open(&chain)?;
        {
            let mut writer = engine.writer()?;
            for (value, stream, parents, label) in steps {
                let _accepted = writer.append(&activity(value, stream, &parents, label)?)?;
            }
        }
        let binding = Binding {
            repository: RepositoryChainBinding {
                workspace_id: name.into(),
                repository_id: name.into(),
                chain: name.into(),
            },
            chain_directory: chain,
            retained_directory: None,
            repository_directory: None,
        };
        let window = read(
            &binding,
            Action::Window {
                view: View::default(),
                position: Position::Latest,
                limit: 200,
            },
        )?;
        drop(windows.insert(name, window));
    }
    std::fs::write(
        root.join("simple-cases.json"),
        serde_json::to_vec_pretty(&windows)?,
    )?;
    Ok(())
}

fn cases() -> Vec<Case> {
    vec![
        (
            "linear",
            vec![
                (1, 1000, vec![], "Main: start"),
                (2, 1000, vec![1], "Main: work"),
                (3, 1000, vec![2], "Main: finish"),
            ],
        ),
        (
            "fork",
            vec![
                (1, 1000, vec![], "Main: spawn child"),
                (2, 1001, vec![1], "Child: start"),
                (3, 1000, vec![1], "Main: continue"),
                (4, 1001, vec![2], "Child: work"),
                (5, 1000, vec![3], "Main: work"),
            ],
        ),
        (
            "join",
            vec![
                (1, 1000, vec![], "Main: spawn child"),
                (2, 1001, vec![1], "Child: start"),
                (3, 1000, vec![1], "Main: continue"),
                (4, 1001, vec![2], "Child: finish"),
                (5, 1000, vec![3, 4], "Main: join child"),
                (6, 1000, vec![5], "Main: finish"),
            ],
        ),
        (
            "siblings",
            vec![
                (1, 1000, vec![], "Main: spawn two children"),
                (2, 1001, vec![1], "Child A: start"),
                (3, 1002, vec![1], "Child B: start"),
                (4, 1000, vec![1], "Main: continue"),
                (5, 1001, vec![2], "Child A: finish"),
                (6, 1002, vec![3], "Child B: finish"),
                (7, 1000, vec![4, 5, 6], "Main: join both children"),
                (8, 1000, vec![7], "Main: finish"),
            ],
        ),
        (
            "nested",
            vec![
                (1, 1000, vec![], "Main: spawn child"),
                (2, 1001, vec![1], "Child: spawn grandchild"),
                (3, 1002, vec![2], "Grandchild: start"),
                (4, 1000, vec![1], "Main: continue"),
                (5, 1002, vec![3], "Grandchild: finish"),
                (6, 1001, vec![2, 5], "Child: join grandchild"),
                (7, 1000, vec![4, 6], "Main: join child"),
                (8, 1000, vec![7], "Main: finish"),
            ],
        ),
        (
            "independent",
            vec![
                (1, 1000, vec![], "Stream A: start"),
                (2, 1001, vec![], "Stream B: start"),
                (3, 1000, vec![1], "Stream A: work"),
                (4, 1001, vec![2], "Stream B: work"),
                (5, 1000, vec![3], "Stream A: finish"),
                (6, 1001, vec![4], "Stream B: finish"),
            ],
        ),
        (
            "passing",
            vec![
                (1, 1000, vec![], "Main: older attachment"),
                (2, 1001, vec![], "Unrelated activity"),
                (3, 1002, vec![1], "Child: exact older attachment"),
            ],
        ),
    ]
}
