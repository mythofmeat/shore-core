use godot::prelude::*;

struct ShoreExtension;

#[gdextension]
unsafe impl ExtensionLibrary for ShoreExtension {}

mod bridge;
