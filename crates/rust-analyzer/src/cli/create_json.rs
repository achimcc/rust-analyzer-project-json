use std::{path::Path, sync::Arc};

use std::fs;
use anyhow::Result;
use crossbeam_channel::{unbounded, Receiver};
use hir::db::DefDatabase;
use ide::{AnalysisHost, Change};
use ide_db::base_db::CrateGraph;
use project_model::{CargoConfig, CargoWorkspace, ProcMacroClient, ProjectManifest, ProjectWorkspace, WorkspaceBuildScripts};
use vfs::{loader::Handle, AbsPath, AbsPathBuf};

use crate::reload::{ProjectFolders, SourceRootConfig};

// Note: Since this type is used by external tools that use rust-analyzer as a library
// what otherwise would be `pub(crate)` has to be `pub` here instead.
pub struct LoadCargoConfig {
    pub load_out_dirs_from_check: bool,
    pub with_proc_macro: bool,
    pub prefill_caches: bool,
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

    let progress = &|_| {};
    match root {
        ProjectManifest::CargoToml(root) => {
            let meta = CargoWorkspace::fetch_metadata(&root, &cargo_config, progress)?;
            let meta = serde_json::to_string(&meta).expect("serialization of change must work");
            fs::write("./meta.json", meta).expect("Unable to write file");
            println!("Metadata written to meta.json");
        }
        ProjectManifest::ProjectJson(root) => {
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
) -> Result<(AnalysisHost, vfs::Vfs, Option<ProcMacroClient>)> {
    let (sender, receiver) = unbounded();
    let mut vfs = vfs::Vfs::default();
    let mut loader = {
        let loader =
            vfs_notify::NotifyHandle::spawn(Box::new(move |msg| sender.send(msg).unwrap()));
        Box::new(loader)
    };

    let proc_macro_client = if load_config.with_proc_macro {
        let path = AbsPathBuf::assert(std::env::current_exe()?);
        Some(ProcMacroClient::extern_process(path, &["proc-macro"]).unwrap())
    } else {
        None
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

    let host =
        load_crate_graph( &mut vfs, &receiver);

    if load_config.prefill_caches {
        host.analysis().prime_caches(|_| {})?;
    }
    Ok((host, vfs, proc_macro_client))
}

fn load_crate_graph(
    vfs: &mut vfs::Vfs,
    receiver: &Receiver<vfs::loader::Message>,
) -> AnalysisHost {
    let lru_cap = std::env::var("RA_LRU_CAP").ok().and_then(|it| it.parse::<usize>().ok());
    let mut host = AnalysisHost::new(lru_cap);
    let mut analysis_change = Change::new();

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
    for file in changes {
        if file.exists() {
            let contents = vfs.file_contents(file.file_id).to_vec();
            if let Ok(text) = String::from_utf8(contents) {
                analysis_change.change_file(file.file_id, Some(Arc::new(text)))
            }
        }
    }

    host
}




