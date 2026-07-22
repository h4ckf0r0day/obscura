// Copyright 2018-2025 the Deno authors. MIT license.

use crate::ModuleLoadResponse;
use crate::ModuleLoader;
use crate::ModuleSource;
use crate::error::CoreError;
use crate::module_specifier::ModuleSpecifier;
use crate::modules::ModuleError;
use crate::modules::ModuleId;
use crate::modules::ModuleLoadId;
use crate::modules::ModuleLoaderError;
use crate::modules::ModuleRequest;
use crate::modules::RequestedModuleType;
use crate::modules::ResolutionKind;
use crate::modules::map::ModuleMap;
use crate::resolve_url;
use futures::future::FutureExt;
use futures::stream::FuturesUnordered;
use futures::stream::Stream;
use futures::stream::TryStreamExt;
use std::cell::RefCell;
use std::collections::HashSet;
use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;
use std::task::Context;
use std::task::Poll;

type ModuleLoadFuture = dyn Future<
  Output = Result<Option<(ModuleRequest, ModuleSource)>, ModuleLoaderError>,
>;

/// Describes the entrypoint of a recursive module load.
#[derive(Debug)]
enum LoadInit {
  /// Main module specifier.
  Main(String),
  /// Module specifier for side module.
  Side(String),
  /// Dynamic import specifier with referrer and expected
  /// module type (which is determined by import assertion).
  DynamicImport(String, String, RequestedModuleType),
}

#[derive(Debug, Eq, PartialEq)]
enum LoadState {
  Init,
  LoadingRoot,
  LoadingImports,
  Done,
}

/// This future is used to implement parallel async module loading.
pub(crate) struct RecursiveModuleLoad {
  pub id: ModuleLoadId,
  pub root_module_id: Option<ModuleId>,
  init: LoadInit,
  state: LoadState,
  module_map_rc: Rc<ModuleMap>,
  pending: FuturesUnordered<Pin<Box<ModuleLoadFuture>>>,
  visited: HashSet<ModuleRequest>,
  visited_as_alias: Rc<RefCell<HashSet<String>>>,
  // The loader is copied from `module_map_rc`, but its reference is cloned
  // ahead of time to avoid already-borrowed errors.
  loader: Rc<dyn ModuleLoader>,
}

impl Drop for RecursiveModuleLoad {
  fn drop(&mut self) {
    self.loader.finish_load();
  }
}

impl RecursiveModuleLoad {
  /// Starts a new asynchronous load of the module graph for given specifier.
  ///
  /// The module corresponding for the given `specifier` will be marked as
  // "the main module" (`import.meta.main` will return `true` for this module).
  pub(crate) fn main(specifier: &str, module_map_rc: Rc<ModuleMap>) -> Self {
    Self::new(LoadInit::Main(specifier.to_string()), module_map_rc)
  }

  /// Starts a new asynchronous load of the module graph for given specifier.
  pub(crate) fn side(specifier: &str, module_map_rc: Rc<ModuleMap>) -> Self {
    Self::new(LoadInit::Side(specifier.to_string()), module_map_rc)
  }

  /// Starts a new asynchronous load of the module graph for given specifier
  /// that was imported using `import()`.
  pub(crate) fn dynamic_import(
    specifier: &str,
    referrer: &str,
    requested_module_type: RequestedModuleType,
    module_map_rc: Rc<ModuleMap>,
  ) -> Self {
    Self::new(
      LoadInit::DynamicImport(
        specifier.to_string(),
        referrer.to_string(),
        requested_module_type,
      ),
      module_map_rc,
    )
  }

  fn new(init: LoadInit, module_map_rc: Rc<ModuleMap>) -> Self {
    let id = module_map_rc.next_load_id();
    let loader = module_map_rc.loader.borrow().clone();
    let requested_module_type = match &init {
      LoadInit::DynamicImport(_, _, module_type) => module_type.clone(),
      _ => RequestedModuleType::None,
    };
    let mut load = Self {
      id,
      root_module_id: None,
      init,
      state: LoadState::Init,
      module_map_rc: module_map_rc.clone(),
      loader,
      pending: FuturesUnordered::new(),
      visited: HashSet::new(),
      visited_as_alias: Default::default(),
    };
    // FIXME(bartlomieju): this seems fishy
    // Ignore the error here, let it be hit in `Stream::poll_next()`.
    if let Ok(root_specifier) = load.resolve_root() {
      if let Some(module_id) =
        module_map_rc.get_id(root_specifier.as_str(), requested_module_type)
      {
        load.root_module_id = Some(module_id);
      }
    }
    load
  }

  fn resolve_root(&self) -> Result<ModuleSpecifier, CoreError> {
    match self.init {
      LoadInit::Main(ref specifier) => {
        self
          .module_map_rc
          .resolve(specifier, ".", ResolutionKind::MainModule)
      }
      LoadInit::Side(ref specifier) => {
        self
          .module_map_rc
          .resolve(specifier, ".", ResolutionKind::Import)
      }
      LoadInit::DynamicImport(ref specifier, ref referrer, _) => self
        .module_map_rc
        .resolve(specifier, referrer, ResolutionKind::DynamicImport),
    }
  }

  pub(crate) async fn prepare(&self) -> Result<(), CoreError> {
    let (module_specifier, maybe_referrer) = match self.init {
      LoadInit::Main(ref specifier) => {
        let spec = self.module_map_rc.resolve(
          specifier,
          ".",
          ResolutionKind::MainModule,
        )?;
        (spec, None)
      }
      LoadInit::Side(ref specifier) => {
        let spec =
          self
            .module_map_rc
            .resolve(specifier, ".", ResolutionKind::Import)?;
        (spec, None)
      }
      LoadInit::DynamicImport(ref specifier, ref referrer, _) => {
        let spec = self.module_map_rc.resolve(
          specifier,
          referrer,
          ResolutionKind::DynamicImport,
        )?;
        (spec, Some(referrer.to_string()))
      }
    };

    self
      .loader
      .prepare_load(&module_specifier, maybe_referrer, self.is_dynamic_import())
      .await
      .map_err(|e| e.into())
  }

  fn is_currently_loading_main_module(&self) -> bool {
    !self.is_dynamic_import()
      && matches!(self.init, LoadInit::Main(..))
      && self.state == LoadState::LoadingRoot
  }

  fn is_dynamic_import(&self) -> bool {
    matches!(self.init, LoadInit::DynamicImport(..))
  }

  pub(crate) fn register_and_recurse(
    &mut self,
    scope: &mut v8::HandleScope,
    module_request: &ModuleRequest,
    module_source: ModuleSource,
  ) -> Result<(), ModuleError> {
    let module_id = self.module_map_rc.new_module(
      scope,
      self.is_currently_loading_main_module(),
      self.is_dynamic_import(),
      module_source,
    )?;

    self.register_and_recurse_inner(module_id, module_request);

    // Update `self.state` however applicable.
    if self.state == LoadState::LoadingRoot {
      self.root_module_id = Some(module_id);
      self.state = LoadState::LoadingImports;
    }
    if self.pending.is_empty() {
      self.state = LoadState::Done;
    }

    Ok(())
  }

  fn register_and_recurse_inner(
    &mut self,
    module_id: usize,
    module_request: &ModuleRequest,
  ) {
    // Recurse the module's imports. There are two cases for each import:
    // 1. If the module is not in the module map, start a new load for it in
    //    `self.pending`. The result of that load should eventually be passed to
    //    this function for recursion.
    // 2. If the module is already in the module map, queue it up to be
    //    recursed synchronously here.
    // This robustly ensures that the whole graph is in the module map before
    // `LoadState::Done` is set.
    let mut already_registered = VecDeque::new();
    already_registered.push_back((module_id, module_request.clone()));
    self.visited.insert(module_request.clone());
    while let Some((module_id, module_request)) = already_registered.pop_front()
    {
      let referrer = &module_request.specifier;
      let imports = self
        .module_map_rc
        .get_requested_modules(module_id)
        .unwrap()
        .clone();
      for module_request in imports {
        if !self.visited.contains(&module_request)
          && !self
            .visited_as_alias
            .borrow()
            .contains(module_request.specifier.as_str())
        {
          match self.module_map_rc.get_id(
            module_request.specifier.as_str(),
            &module_request.requested_module_type,
          ) {
            Some(module_id) => {
              already_registered.push_back((module_id, module_request.clone()));
            }
            _ => {
              let request = module_request.clone();
              let visited_as_alias = self.visited_as_alias.clone();
              let referrer = referrer.clone();
              let loader = self.loader.clone();
              let is_dynamic_import = self.is_dynamic_import();
              let requested_module_type = request.requested_module_type.clone();
              let fut = async move {
                // `visited_as_alias` unlike `visited` is checked as late as
                // possible because it can only be populated after completed
                // loads, meaning a duplicate load future may have already been
                // dispatched before we know it's a duplicate.
                if visited_as_alias
                  .borrow()
                  .contains(request.specifier.as_str())
                {
                  return Ok(None);
                }
                let load_response = loader.load(
                  &request.specifier,
                  Some(&referrer),
                  is_dynamic_import,
                  requested_module_type,
                );

                let load_result = match load_response {
                  ModuleLoadResponse::Sync(result) => result,
                  ModuleLoadResponse::Async(fut) => fut.await,
                };
                if let Ok(source) = &load_result {
                  if let Some(found_specifier) = &source.module_url_found {
                    visited_as_alias
                      .borrow_mut()
                      .insert(found_specifier.as_str().to_string());
                  }
                }
                load_result.map(|s| Some((request, s)))
              };
              self.pending.push(fut.boxed_local());
            }
          }
          self.visited.insert(module_request);
        }
      }
    }
  }
}

impl Stream for RecursiveModuleLoad {
  type Item = Result<(ModuleRequest, ModuleSource), CoreError>;

  fn poll_next(
    self: Pin<&mut Self>,
    cx: &mut Context,
  ) -> Poll<Option<Self::Item>> {
    let inner = self.get_mut();
    // IMPORTANT: Do not borrow `inner.module_map_rc` here. It may not be
    // available.
    match inner.state {
      LoadState::Init => {
        let module_specifier = match inner.resolve_root() {
          Ok(url) => url,
          Err(error) => {
            return Poll::Ready(Some(Err(error)));
          }
        };
        let requested_module_type = match &inner.init {
          LoadInit::DynamicImport(_, _, module_type) => module_type.clone(),
          _ => RequestedModuleType::None,
        };
        let module_request = ModuleRequest {
          specifier: module_specifier.clone(),
          requested_module_type: requested_module_type.clone(),
        };
        let load_fut = if let Some(module_id) = inner.root_module_id {
          // If the inner future is already in the map, we might be done (assuming there are no pending
          // loads).
          inner.register_and_recurse_inner(module_id, &module_request);
          if inner.pending.is_empty() {
            inner.state = LoadState::Done;
          } else {
            inner.state = LoadState::LoadingImports;
          }
          // Internally re-poll using the new state to avoid spinning the event loop again.
          return Self::poll_next(Pin::new(inner), cx);
        } else {
          let maybe_referrer = match inner.init {
            LoadInit::DynamicImport(_, ref referrer, _) => {
              resolve_url(referrer).ok()
            }
            _ => None,
          };
          let loader = inner.loader.clone();
          let is_dynamic_import = inner.is_dynamic_import();
          let requested_module_type = requested_module_type.clone();
          async move {
            let load_response = loader.load(
              &module_specifier,
              maybe_referrer.as_ref(),
              is_dynamic_import,
              requested_module_type,
            );
            let result = match load_response {
              ModuleLoadResponse::Sync(result) => result,
              ModuleLoadResponse::Async(fut) => fut.await,
            };
            result.map(|s| Some((module_request, s)))
          }
          .boxed_local()
        };
        inner.pending.push(load_fut);
        inner.state = LoadState::LoadingRoot;
        inner.try_poll_next_unpin(cx)
      }
      LoadState::LoadingRoot | LoadState::LoadingImports => {
        // Poll the futures that load the source code of the modules
        match inner.pending.try_poll_next_unpin(cx)? {
          Poll::Ready(None) => unreachable!(),
          Poll::Ready(Some(None)) => {
            // The future resolves to None when loading an already visited redirect
            if inner.pending.is_empty() {
              inner.state = LoadState::Done;
              Poll::Ready(None)
            } else {
              // Force re-poll to make sure new ModuleLoadFuture's wakers are registered
              inner.try_poll_next_unpin(cx)
            }
          }
          Poll::Ready(Some(Some(info))) => Poll::Ready(Some(Ok(info))),
          Poll::Pending => Poll::Pending,
        }
      }
      LoadState::Done => Poll::Ready(None),
    }
  }
}
