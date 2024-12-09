use anyhow::Result;
use futures::Future;
use next_core::{
    next_app::ClientReferencesChunks,
    next_client_reference::{ClientReferenceType, EcmascriptClientReferenceModule},
    next_dynamic::NextDynamicEntryModule,
};
use serde::{Deserialize, Serialize};
use swc_core::ecma::{
    ast::{CallExpr, Callee, Expr, Ident, Lit},
    visit::{Visit, VisitWith},
};
use turbo_rcstr::RcStr;
use turbo_tasks::{
    debug::ValueDebugFormat, trace::TraceRawVcs, FxIndexMap, ResolvedVc, TryFlatJoinIterExt,
    TryJoinIterExt, Value, Vc,
};
use turbopack_core::{
    chunk::{
        availability_info::AvailabilityInfo, ChunkItem, ChunkItemExt, ChunkableModule,
        ChunkingContext, ModuleId,
    },
    context::AssetContext,
    module::Module,
    output::{OutputAsset, OutputAssets},
    reference::ModuleReference,
    reference_type::EcmaScriptModulesReferenceSubType,
    resolve::{origin::PlainResolveOrigin, parse::Request, pattern::Pattern},
};
use turbopack_ecmascript::{parse::ParseResult, resolve::esm_resolve, EcmascriptParsable};

use crate::module_graph::SingleModuleGraph;

pub(crate) async fn collect_next_dynamic_chunks(
    chunking_context: Vc<Box<dyn ChunkingContext>>,
    dynamic_import_entries: &[(
        ResolvedVc<NextDynamicEntryModule>,
        Option<ClientReferenceType>,
    )],
    client_reference_chunks: Option<&ClientReferencesChunks>,
) -> Result<Vc<DynamicImportedChunks>> {
    let dynamic_import_chunks = dynamic_import_entries
        .iter()
        .map(|(dynamic_entry, parent_client_reference)| async move {
            let module = ResolvedVc::upcast::<Box<dyn ChunkableModule>>(*dynamic_entry);

            // This is the availability info for the parent chunk group, i.e. the client reference
            // containing the next/dynamic imports
            let availability_info = if let Some(parent_client_reference) = parent_client_reference {
                client_reference_chunks
                    .unwrap()
                    .client_component_client_chunks
                    .get(parent_client_reference)
                    .unwrap()
                    .1
            } else {
                // In pages router, there are no parent_client_reference and no
                // client_reference_chunks
                AvailabilityInfo::Root
            };

            let async_loader =
                chunking_context.async_loader_chunk_item(*module, Value::new(availability_info));
            let async_chunk_group = async_loader
                .references()
                .await?
                .iter()
                .map(|reference| reference.resolve_reference().primary_output_assets())
                .try_join()
                .await?;
            let async_chunk_group: Vec<ResolvedVc<Box<dyn OutputAsset>>> =
                async_chunk_group.iter().flatten().copied().collect();

            let module_id = dynamic_entry
                .as_chunk_item(Vc::upcast(chunking_context))
                .id()
                .to_resolved()
                .await?;

            Ok((
                *dynamic_entry,
                (module_id, ResolvedVc::cell(async_chunk_group)),
            ))
        })
        .try_join()
        .await?;

    Ok(Vc::cell(FxIndexMap::from_iter(dynamic_import_chunks)))
}

/// Returns a mapping of the dynamic imports for the module, if the import is
/// wrapped in `next/dynamic`'s `dynamic()`. Refer [documentation](https://nextjs.org/docs/pages/building-your-application/optimizing/lazy-loading#with-named-exports) for the usecases.
///
/// If an import is specified as dynamic, next.js does few things:
/// - Runs a next_dynamic [transform to the source file](https://github.com/vercel/next.js/blob/ae1b89984d26b2af3658001fa19a19e1e77c312d/packages/next-swc/crates/next-transform-dynamic/src/lib.rs#L22)
///   - This transform will [inject `loadableGenerated` property](https://github.com/vercel/next.js/blob/ae1b89984d26b2af3658001fa19a19e1e77c312d/packages/next-swc/crates/next-transform-dynamic/tests/fixture/wrapped-import/output-webpack-dev.js#L5),
///     which contains the list of the import ids in the form of `${origin} -> ${imported}`.
/// - Emits `react-loadable-manifest.json` which contains the mapping of the import ids to the chunk
///   ids.
///   - Webpack: [implementation](https://github.com/vercel/next.js/blob/ae1b89984d26b2af3658001fa19a19e1e77c312d/packages/next/src/build/webpack/plugins/react-loadable-plugin.ts)
///   - Turbopack: [implementation 1](https://github.com/vercel/next.js/pull/56389/files#diff-3cac9d9bfe73e0619e6407f21f6fe652da0719d0ec9074ff813ad3e416d0eb1a),
///     [implementation 2](https://github.com/vercel/next.js/pull/56389/files#diff-791951bbe1fa09bcbad9be9173412d0848168f7d658758f11b6e8888a021552c),
///     [implementation 3](https://github.com/vercel/next.js/pull/56389/files#diff-c33f6895801329243dd3f627c69da259bcab95c2c9d12993152842591931ff01R557)
/// - When running an application,
///    - Server reads generated `react-loadable-manifest.json`, sets dynamicImportIds with the mapping of the import ids, and dynamicImports to the actual corresponding chunks.
///         [implementation 1](https://github.com/vercel/next.js/blob/ad42b610c25b72561ad367b82b1c7383fd2a5dd2/packages/next/src/server/load-components.ts#L119),
///         [implementation 2](https://github.com/vercel/next.js/blob/ad42b610c25b72561ad367b82b1c7383fd2a5dd2/packages/next/src/server/render.tsx#L1417C7-L1420)
///    - Server embeds those into __NEXT_DATA__ and [send to the client.](https://github.com/vercel/next.js/blob/ad42b610c25b72561ad367b82b1c7383fd2a5dd2/packages/next/src/server/render.tsx#L1453)
///    - When client boots up, pass it to the [client preload](https://github.com/vercel/next.js/blob/ad42b610c25b72561ad367b82b1c7383fd2a5dd2/packages/next/src/client/index.tsx#L943)
///    - Loadable runtime [injects preload fn](https://github.com/vercel/next.js/blob/ad42b610c25b72561ad367b82b1c7383fd2a5dd2/packages/next/src/shared/lib/loadable.shared-runtime.tsx#L281)
///      to wait until all the dynamic components are being loaded, this ensures hydration mismatch
///      won't occur
#[turbo_tasks::function]
pub async fn build_dynamic_imports_map_for_module(
    client_asset_context: Vc<Box<dyn AssetContext>>,
    server_module: ResolvedVc<Box<dyn Module>>,
) -> Result<Vc<OptionDynamicImportsMap>> {
    let Some(ecmascript_asset) =
        ResolvedVc::try_sidecast::<Box<dyn EcmascriptParsable>>(server_module).await?
    else {
        return Ok(Vc::cell(None));
    };

    // https://github.com/vercel/next.js/pull/56389#discussion_r1349336374
    // don't emit specific error as we expect there's a parse error already reported
    let ParseResult::Ok { program, .. } = &*ecmascript_asset.failsafe_parse().await? else {
        return Ok(Vc::cell(None));
    };

    // Reading the Program AST, collect raw imported module str if it's wrapped in
    // dynamic()
    let mut visitor = DynamicImportVisitor::new();
    program.visit_with(&mut visitor);

    if visitor.import_sources.is_empty() {
        return Ok(Vc::cell(None));
    }

    let mut import_sources = vec![];
    for import in visitor.import_sources.drain(..) {
        // Using the given `Module` which is the origin of the dynamic import, trying to
        // resolve the module that is being imported.
        let dynamic_imported_resolved_module = *esm_resolve(
            Vc::upcast(PlainResolveOrigin::new(
                client_asset_context,
                server_module.ident().path(),
            )),
            Request::parse(Value::new(Pattern::Constant(import.clone()))),
            Value::new(EcmaScriptModulesReferenceSubType::DynamicImport),
            false,
            None,
        )
        .first_module()
        .await?;

        if let Some(dynamic_imported_resolved_module) = dynamic_imported_resolved_module {
            import_sources.push((import, dynamic_imported_resolved_module));
        }
    }

    Ok(Vc::cell(Some(ResolvedVc::cell((
        server_module,
        import_sources,
    )))))
}

/// A visitor to check if there's import to `next/dynamic`, then collecting the
/// import wrapped with dynamic() via CollectImportSourceVisitor.
struct DynamicImportVisitor {
    dynamic_ident: Option<Ident>,
    pub import_sources: Vec<RcStr>,
}

impl DynamicImportVisitor {
    fn new() -> Self {
        Self {
            import_sources: vec![],
            dynamic_ident: None,
        }
    }
}

impl Visit for DynamicImportVisitor {
    fn visit_import_decl(&mut self, decl: &swc_core::ecma::ast::ImportDecl) {
        // find import decl from next/dynamic, i.e import dynamic from 'next/dynamic'
        if decl.src.value == *"next/dynamic" {
            if let Some(specifier) = decl.specifiers.first().and_then(|s| s.as_default()) {
                self.dynamic_ident = Some(specifier.local.clone());
            }
        }
    }

    fn visit_call_expr(&mut self, call_expr: &CallExpr) {
        // Collect imports if the import call is wrapped in the call dynamic()
        if let Callee::Expr(ident) = &call_expr.callee {
            if let Expr::Ident(ident) = &**ident {
                if let Some(dynamic_ident) = &self.dynamic_ident {
                    if ident.sym == *dynamic_ident.sym {
                        let mut collect_import_source_visitor = CollectImportSourceVisitor::new();
                        call_expr.visit_children_with(&mut collect_import_source_visitor);

                        if let Some(import_source) = collect_import_source_visitor.import_source {
                            self.import_sources.push(import_source);
                        }
                    }
                }
            }
        }

        call_expr.visit_children_with(self);
    }
}

/// A visitor to collect import source string from import('path/to/module')
struct CollectImportSourceVisitor {
    import_source: Option<RcStr>,
}

impl CollectImportSourceVisitor {
    fn new() -> Self {
        Self {
            import_source: None,
        }
    }
}

impl Visit for CollectImportSourceVisitor {
    fn visit_call_expr(&mut self, call_expr: &CallExpr) {
        // find import source from import('path/to/module')
        // [NOTE]: Turbopack does not support webpack-specific comment directives, i.e
        // import(/* webpackChunkName: 'hello1' */ '../../components/hello3')
        // Renamed chunk in the comment will be ignored.
        if let Callee::Import(_import) = call_expr.callee {
            if let Some(arg) = call_expr.args.first() {
                if let Expr::Lit(Lit::Str(str_)) = &*arg.expr {
                    self.import_source = Some(str_.value.as_str().into());
                }
            }
        }

        // Don't need to visit children, we expect import() won't have any
        // nested calls as dynamic() should be statically analyzable import.
    }
}

pub type DynamicImportedModules = Vec<(RcStr, ResolvedVc<Box<dyn Module>>)>;
pub type DynamicImportedOutputAssets = Vec<(RcStr, ResolvedVc<OutputAssets>)>;

/// A struct contains mapping for the dynamic imports to construct chunk per
/// each individual module (Origin Module, Vec<(ImportSourceString, Module)>)
#[turbo_tasks::value(transparent)]
pub struct DynamicImportsMap(pub (ResolvedVc<Box<dyn Module>>, DynamicImportedModules));

/// An Option wrapper around [DynamicImportsMap].
#[turbo_tasks::value(transparent)]
pub struct OptionDynamicImportsMap(Option<ResolvedVc<DynamicImportsMap>>);

#[turbo_tasks::value(transparent)]
#[derive(Default)]
pub struct DynamicImportedChunks(
    pub  FxIndexMap<
        ResolvedVc<NextDynamicEntryModule>,
        (ResolvedVc<ModuleId>, ResolvedVc<OutputAssets>),
    >,
);

/// "app/client.js [app-ssr] (ecmascript)" ->
///      [("./dynamic", "app/dynamic.js [app-client] (ecmascript)")])]
#[turbo_tasks::value(transparent)]
pub struct DynamicImports(pub FxIndexMap<ResolvedVc<Box<dyn Module>>, DynamicImportedModules>);

#[derive(Clone, PartialEq, Eq, ValueDebugFormat, Serialize, Deserialize, TraceRawVcs)]
pub enum DynamicImportEntriesMapType {
    DynamicEntry(ResolvedVc<NextDynamicEntryModule>),
    ClientReference(ResolvedVc<EcmascriptClientReferenceModule>),
}

#[turbo_tasks::value(transparent)]
pub struct DynamicImportEntries(
    pub FxIndexMap<ResolvedVc<Box<dyn Module>>, DynamicImportEntriesMapType>,
);

#[turbo_tasks::function]
pub async fn map_next_dynamic(graph: Vc<SingleModuleGraph>) -> Result<Vc<DynamicImportEntries>> {
    let actions = graph
        .await?
        .enumerate_nodes()
        .map(|(_, node)| async move {
            let module = node.module;
            let layer = node.layer.as_ref();
            if layer.is_some_and(|layer| &**layer == "app-client" || &**layer == "client") {
                if let Some(dynamic_entry_module) =
                    ResolvedVc::try_downcast_type::<NextDynamicEntryModule>(module).await?
                {
                    return Ok(Some((
                        module,
                        DynamicImportEntriesMapType::DynamicEntry(dynamic_entry_module),
                    )));
                }
            }
            // TODO add this check once these modules have the correct layer
            // if layer.is_some_and(|layer| &**layer == "app-rsc") {
            if let Some(client_reference_module) =
                ResolvedVc::try_downcast_type::<EcmascriptClientReferenceModule>(module).await?
            {
                return Ok(Some((
                    module,
                    DynamicImportEntriesMapType::ClientReference(client_reference_module),
                )));
            }
            // }
            Ok(None)
        })
        .try_flat_join()
        .await?;
    Ok(Vc::cell(actions.into_iter().collect()))
}
