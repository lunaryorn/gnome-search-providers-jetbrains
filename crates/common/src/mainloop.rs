// Copyright Sebastian Wiesner <sebastian@swsnr.de>
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.

//! Mainloop utilities for dbus serch providers.

use std::os::unix::io::AsRawFd;

use log::{debug, error, trace};
use thiserror::Error;

use gio::glib;
use gio::glib::source::SourceId;

/// An error occurred while starting the main loop.
#[derive(Error, Debug)]
pub enum MainLoopError {
    /// Failed to acquire the main Glib context.
    #[error("Failed to acquire context")]
    FailedToAcquireContext,
}

/// Add `connection` as source to the default Glib mainloop.
///
/// Invoke `on_message` for every message received from `connection`.
///
/// `on_message` is not required to be `Send` but the calling thread needs to
/// own the main context to make sure that `on_message` remains on the main thread.
pub fn source_add_connection_local<F: FnMut(zbus::Message) + 'static>(
    connection: zbus::Connection,
    mut on_message: F,
) -> SourceId {
    let conditions = glib::IOCondition::IN | glib::IOCondition::PRI;
    debug!(
        "Watching connection fd {} for conditions {:?}",
        connection.as_raw_fd(),
        conditions
    );
    glib::source::unix_fd_add_local(connection.as_raw_fd(), conditions, move |_, condition| {
        trace!("Connection entered IO condition {:?}", condition);
        match connection.receive_message() {
            Ok(message) => on_message(message),
            Err(err) => error!("Failed to process message: {:#}", err),
        }
        glib::Continue(true)
    })
}

/// Connect to session bus, acquire the given name on the bus, and start handling messages.
pub fn run_dbus_loop<F: FnMut(zbus::Message) + 'static>(
    connection: zbus::Connection,
    on_message: F,
) -> Result<(), MainLoopError> {
    trace!("Acquire main context");
    let context = glib::MainContext::default();
    let guard = context
        .acquire()
        .map_err(|_| MainLoopError::FailedToAcquireContext)?;
    let mainloop = glib::MainLoop::new(Some(&context), false);

    source_add_connection_local(connection, on_message);

    trace!("Listening for SIGTERM");
    glib::source::unix_signal_add(
        libc::SIGTERM,
        glib::clone!(@strong mainloop =>  move || {
            debug!("Terminated, quitting mainloop");
            mainloop.quit();
            glib::Continue(false)
        }),
    );

    trace!("Listening for SIGINT");
    glib::source::unix_signal_add(
        libc::SIGINT,
        glib::clone!(@strong mainloop =>  move || {
            debug!("Interrupted, quitting mainloop");
            mainloop.quit();
            glib::Continue(false)
        }),
    );

    trace!("mainloop run");
    mainloop.run();
    // We no longer require the main context.
    drop(guard);
    Ok(())
}
