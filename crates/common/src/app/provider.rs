// Copyright Sebastian Wiesner <sebastian@swsnr.de>
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.

//! Search providers for apps.

use std::collections::HashMap;
use std::fmt::Debug;

use indexmap::IndexMap;
use tracing::field;
use tracing::instrument;
use zbus::dbus_interface;
use zbus::zvariant;

use crate::app::*;
use crate::matching::*;
use crate::source::*;

/// A search provider for recent items.
#[derive(Debug)]
pub struct AppItemSearchProvider<S: AsyncItemsSource<AppLaunchItem>> {
    launcher: AppLaunchClient,
    app: App,
    source: S,
    items: IdMap<AppLaunchItem>,
}

impl<S: AsyncItemsSource<AppLaunchItem>> AppItemSearchProvider<S> {
    /// The app managed by this search provider.
    pub fn app(&self) -> &App {
        &self.app
    }
}

impl<S: AsyncItemsSource<AppLaunchItem>> AppItemSearchProvider<S> {
    /// Create a new search provider for recent items of `app`.
    ///
    /// Uses the given `source` to load recent items.
    pub fn new(app: App, source: S, launcher: AppLaunchClient) -> Self {
        Self {
            launcher,
            app,
            source,
            items: IndexMap::new(),
        }
    }
}

/// The DBus interface of the search provider.
///
/// See <https://developer.gnome.org/SearchProvider/> for information.
#[dbus_interface(name = "org.gnome.Shell.SearchProvider2")]
impl<S: AsyncItemsSource<AppLaunchItem> + Send + Sync + 'static> AppItemSearchProvider<S> {
    /// Starts a search.
    ///
    /// This function is called when a new search is started. It gets an array of search terms as arguments,
    /// and should return an array of result IDs. gnome-shell will call GetResultMetas for (some) of these result
    /// IDs to get details about the result that can be be displayed in the result list.
    #[instrument(skip(self), fields(app_id = field::debug(self.app.id())))]
    async fn get_initial_result_set(&mut self, terms: Vec<&str>) -> zbus::fdo::Result<Vec<String>> {
        debug!("Searching for {:?} of {}", terms, self.app.id());
        self.items = self.source.find_recent_items().await.map_err(|error| {
            error!(
                "Failed to update recent items for {}: {:#}",
                self.app.id(),
                error,
            );
            zbus::fdo::Error::Failed(format!(
                "Failed to update recent items for {}: {:#}",
                self.app.id(),
                error
            ))
        })?;

        let ids = find_matching_items(self.items.iter(), terms.as_slice())
            .into_iter()
            .map(String::to_owned)
            .collect();
        debug!("Found ids {:?} for {}", ids, self.app.id());
        Ok(ids)
    }

    /// Refine an ongoing search.
    ///
    /// This function is called to refine the initial search results when the user types more characters in the search entry.
    /// It gets the previous search results and the current search terms as arguments, and should return an array of result IDs,
    /// just like GetInitialResultSet.
    #[instrument(skip(self), fields(app_id = field::debug(self.app.id())))]
    fn get_subsearch_result_set(
        &self,
        previous_results: Vec<&str>,
        terms: Vec<&str>,
    ) -> Vec<String> {
        debug!(
            "Searching for {:?} in {:?} of {}",
            terms,
            previous_results,
            self.app.id()
        );
        let candidates = previous_results
            .iter()
            .filter_map(|&id| self.items.get(id).map(|p| (id, p)));

        let ids = find_matching_items(candidates, terms.as_slice())
            .into_iter()
            .map(|s| s.to_owned())
            .collect();
        debug!("Found ids {:?} for {}", ids, self.app.id());
        ids
    }

    /// Get metadata for results.
    ///
    /// This function is called to obtain detailed information for results.
    /// It gets an array of result IDs as arguments, and should return a matching array of dictionaries
    /// (ie one a{sv} for each passed-in result ID).
    ///
    /// The following pieces of information should be provided for each result:
    //
    //  - "id": the result ID
    //  - "name": the display name for the result
    //  - "icon": a serialized GIcon (see g_icon_serialize()), or alternatively,
    //  - "gicon": a textual representation of a GIcon (see g_icon_to_string()), or alternatively,
    //  - "icon-data": a tuple of type (iiibiiay) describing a pixbuf with width, height, rowstride, has-alpha, bits-per-sample, and image data
    //  - "description": an optional short description (1-2 lines)
    #[instrument(skip(self), fields(app_id = field::debug(self.app.id())))]
    fn get_result_metas(&self, results: Vec<String>) -> Vec<HashMap<String, zvariant::Value>> {
        debug!("Getting meta info for {:?}", results);
        let metas = results
            .iter()
            .filter_map(|id| {
                self.items.get(id).map(|item| {
                    debug!("Compiling meta info for {}", id);
                    debug!("Using icon {} for id {}", self.app.icon(), id);

                    let mut meta: HashMap<String, zvariant::Value> = HashMap::new();
                    meta.insert("id".to_string(), id.clone().into());
                    meta.insert("name".to_string(), (&item.name).into());
                    meta.insert("gicon".to_string(), self.app.icon().to_string().into());
                    meta.insert("description".to_string(), item.uri.clone().into());
                    meta
                })
            })
            .collect();

        debug!("Return meta info {:?}", &metas);
        metas
    }

    /// Activate an individual result.
    ///
    /// This function is called when the user clicks on an individual result to open it in the application.
    /// The arguments are the result ID, the current search terms and a timestamp.
    ///
    /// Launches the underlying app with the path to the selected item.
    #[instrument(skip(self), fields(app_id = field::debug(self.app.id())))]
    async fn activate_result(
        &self,
        id: &str,
        terms: Vec<&str>,
        timestamp: u32,
    ) -> zbus::fdo::Result<()> {
        debug!("Activating result {} for {:?} at {}", id, terms, timestamp);
        if let Some(item) = self.items.get(id) {
            info!("Launching recent item {:?} for {}", item, self.app.id());
            self.launcher
                .launch_uri(self.app.id().clone(), item.uri.clone())
                .await
                .map_err(|error| {
                    error!(
                        "Failed to launch app {} for {:?}: {}",
                        self.app.id(),
                        item.uri,
                        error
                    );
                    zbus::fdo::Error::Failed(format!(
                        "Failed to launch app {} for {}: {}",
                        self.app.id(),
                        item.uri,
                        error
                    ))
                })
        } else {
            error!("Item with ID {} not found for {}", id, self.app.id());
            Err(zbus::fdo::Error::Failed(format!("Result {} not found", id)))
        }
    }

    /// Launch a search within the App.
    ///
    /// This function is called when the user clicks on the provider icon to display more search results in the application.
    /// The arguments are the current search terms and a timestamp.
    ///
    /// Currently it simply launches the app without any arguments.
    #[instrument(skip(self), fields(app_id = field::debug(self.app.id())))]
    async fn launch_search(&self, _terms: Vec<String>, _timestamp: u32) -> zbus::fdo::Result<()> {
        info!("Launching app {} directly", self.app.id());
        self.launcher
            .launch_app(self.app.id().clone())
            .await
            .map_err(|error| {
                error!("Failed to launch app {}: {}", self.app.id(), error);
                zbus::fdo::Error::Failed(format!(
                    "Failed to launch app {}: {}",
                    self.app.id(),
                    error
                ))
            })
    }
}
