use std::collections::HashMap;

use anyhow::Result;
use next_core::next_manifests::LoadableManifest;
use turbo_rcstr::RcStr;
use turbo_tasks::{TryFlatJoinIterExt, ValueToString, Vc};
use turbo_tasks_fs::{File, FileContent, FileSystemPath};
use turbopack_core::{
    asset::AssetContent, output::OutputAsset, virtual_output::VirtualOutputAsset,
};

use crate::dynamic_imports::DynamicImportedChunks;

#[turbo_tasks::function]
pub async fn create_react_loadable_manifest(
    dynamic_import_entries: Vc<DynamicImportedChunks>,
    client_relative_path: Vc<FileSystemPath>,
    output_path: Vc<FileSystemPath>,
) -> Result<Vc<Box<dyn OutputAsset>>> {
    let dynamic_import_entries = &*dynamic_import_entries.await?;

    let mut loadable_manifest: HashMap<RcStr, LoadableManifest> = Default::default();

    for (_, (module_id, chunk_output)) in dynamic_import_entries.into_iter() {
        let chunk_output = chunk_output.await?;

        let id = module_id.to_string().await?.clone_value();

        let client_relative_path_value = client_relative_path.await?;
        let files = chunk_output
            .iter()
            .map(move |&file| {
                let client_relative_path_value = client_relative_path_value.clone();
                async move {
                    Ok(client_relative_path_value
                        .get_path_to(&*file.ident().path().await?)
                        .map(|path| path.into()))
                }
            })
            .try_flat_join()
            .await?;

        let manifest_item = LoadableManifest {
            id: id.clone(),
            files,
        };

        loadable_manifest.insert(id, manifest_item);
    }

    let loadable_manifest = VirtualOutputAsset::new(
        output_path,
        AssetContent::file(
            FileContent::Content(File::from(serde_json::to_string_pretty(
                &loadable_manifest,
            )?))
            .cell(),
        ),
    );

    Ok(Vc::upcast(loadable_manifest))
}
