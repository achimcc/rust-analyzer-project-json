use std::{path::Path, sync::Arc};

use std::fs;
use anyhow::Result;
use crossbeam_channel::{unbounded, Receiver};
use hir::db::DefDatabase;
use ide::{AnalysisHost, FileId};
use project_model::{CargoConfig, CargoWorkspace, ProjectManifest, ProjectWorkspace, WorkspaceBuildScripts};
use vfs::{loader::Handle, AbsPathBuf};
use cargo_metadata::Metadata;
use serde::{Serialize, Deserialize};

use crate::reload::ProjectFolders;

// Note: Since this type is used by external tools that use rust-analyzer as a library
// what otherwise would be `pub(crate)` has to be `pub` here instead.
pub struct LoadCargoConfig {
    pub load_out_dirs_from_check: bool,
    pub with_proc_macro: bool,
    pub prefill_caches: bool,
}

#[derive(Serialize, Deserialize)]
pub struct ChangeJson {
    pub meta: Metadata,
    #[serde(serialize_with = "serialize_files")]
    #[serde(deserialize_with = "deserialize_files")]
    pub files: Vec<(FileId, Option<Arc<String>>)>,
}

fn serialize_files<S>(files: &Vec<(FileId, Option<Arc<String>>)>, ser: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    ser.collect_seq(files.iter().map(|(id, txt)|{(id.to_owned().0, txt.as_ref().unwrap().as_str().to_owned())}))
}

fn deserialize_files<'de, D>(de: D) -> Result<Vec<(FileId, Option<Arc<String>>)>, D::Error>
where
    D: serde::de::Deserializer<'de>,
{
    let v: Vec<(u32, String)> = Vec::deserialize(de)?;
    let v = v.into_iter().map(|(id, txt)| (FileId {0: id }, Some(Arc::from(String::from(txt))))).collect();
    Ok(v)
}

// Note: Since this function is used by external tools that use rust-analyzer as a library
// what otherwise would be `pub(crate)` has to be `pub` here instead.
pub fn create_json_file(
    root: &Path,
) -> Result<()> {
    let mut cargo_config = CargoConfig::default();
    cargo_config.no_sysroot = false;

    let root = AbsPathBuf::assert(std::env::current_dir()?.join(root));
    let root = ProjectManifest::discover_single(&root)?;

    let ws = ProjectWorkspace::load(root.clone(), &cargo_config, &|_| {})?;

    let progress = &|_| {};
    match root {
        ProjectManifest::CargoToml(root) => {
            let meta = CargoWorkspace::fetch_metadata(&root, &cargo_config, progress)?;
            // let meta = serde_json::to_string(&meta).expect("serialization of change must work");
            // fs::write("./meta.json", meta).expect("Unable to write file");
            let load_config = LoadCargoConfig {
                load_out_dirs_from_check: true,
                with_proc_macro: false,
                prefill_caches: false,
            };
            let files = load_workspace(ws, &cargo_config, &load_config, &|_| {}).unwrap();
            let change = ChangeJson {
                meta,
                files
            };
            let change = serde_json::to_string(&change).expect("serialization of change must work");
            let _deserialized_change: ChangeJson = serde_json::from_str(&change).expect("deserialization of change must work");
            fs::write("./change.json", change).expect("Unable to write file");
            println!("Metadata written to meta.json");
        }
        ProjectManifest::ProjectJson(_) => {
            println!("Cargo.toml needed!");
        }
    }
    Ok(())
}

// Note: Since this function is used by external tools that use rust-analyzer as a library
// what otherwise would be `pub(crate)` has to be `pub` here instead.
//
// The reason both, `load_workspace_at` and `load_workspace` are `pub` is that some of
// these tools need access to `ProjectWorkspace`, too, which `load_workspace_at` hides.
pub fn load_workspace(
    mut ws: ProjectWorkspace,
    cargo_config: &CargoConfig,
    load_config: &LoadCargoConfig,
    progress: &dyn Fn(String),
) -> Result<Vec<(FileId, Option<Arc<String>>)>> {
    let (sender, receiver) = unbounded();
    let mut vfs = vfs::Vfs::default();
     
    let mut loader = {
        let loader =
            vfs_notify::NotifyHandle::spawn(Box::new(move |msg| sender.send(msg).unwrap()));
        Box::new(loader)
    };

    ws.set_build_scripts(if load_config.load_out_dirs_from_check {
        ws.run_build_scripts(cargo_config, progress)?
    } else {
        WorkspaceBuildScripts::default()
    });
    

    let project_folders = ProjectFolders::new(&[ws], &[]);
 
    loader.set_config(vfs::loader::Config {
        load: project_folders.load,
        watch: vec![],
        version: 0,
    });

    let files =
        load_files( &mut vfs, &receiver); 

    Ok(files)
}

fn load_files(
    vfs: &mut vfs::Vfs,
    receiver: &Receiver<vfs::loader::Message>,
) -> Vec<(FileId, Option<Arc<String>>)> {
    let lru_cap = std::env::var("RA_LRU_CAP").ok().and_then(|it| it.parse::<usize>().ok());
    let mut host = AnalysisHost::new(lru_cap);

    host.raw_database_mut().set_enable_proc_attr_macros(true);

    // wait until Vfs has loaded all roots
    for task in receiver {
        match task {
            vfs::loader::Message::Progress { n_done, n_total, config_version: _ } => {
                if n_done == n_total {
                    break;
                }
            }
            vfs::loader::Message::Loaded { files } => {
                for (path, contents) in files {
                    vfs.set_file_contents(path.into(), contents);
                }
            }
        }
    }
    let changes = vfs.take_changes();

    let mut files: Vec<(FileId, Option<Arc<String>>)> = Vec::new();

    for file in changes {
        if file.exists() {
            let contents = vfs.file_contents(file.file_id).to_vec();
            if let Ok(text) = String::from_utf8(contents) {
                files.push((file.file_id, Some(Arc::new(text))))
            }
        }
    }

    files
}




