use super::*;
/// The structure types registered at startup, for the executable to hand to
/// the engine's `StructureRegistry`.
pub(crate) fn installed_structure_type_list()
-> &'static [crate::world::structure_type::StructureTypeInfo] {
    installed().map_or(&[], |installed| installed.structure_types.as_slice())
}

/// The pool element types registered at startup, for the executable to hand
/// to the engine's element type table.
pub(crate) fn installed_pool_element_type_list()
-> &'static [crate::world::pool_element::PoolElementTypeInfo] {
    installed().map_or(&[], |installed| installed.pool_element_types.as_slice())
}

impl dyn Registrar + '_ {
    /// Register structure type `name` (`tower` or `mymod:tower`), the way
    /// NeoForge's registries `STRUCTURE_TYPE` and `STRUCTURE_PIECE` register
    /// once at startup: a data pack structure with `"type": "modid:tower"`
    /// then generates by `ty`'s code — its own start and pieces, placed by the
    /// game's own spacing, biome and terrain-adaptation checks. The type's id
    /// is in a namespace owned by the calling mod's manifest (`mymod:tower`); another mod's
    /// namespace, an empty name and an id registered twice refuse the mods at
    /// startup, naming the mod. `ty`'s `load_piece` is its saved pieces'
    /// loader: keep the type for as long as the game runs.
    pub fn register_structure_type<T: crate::StructureType>(&mut self, name: &str, ty: T) {
        self.add_structure_type(name, Arc::new(ty));
    }
    /// Register pool element type `name` (`corridor` or `mymod:corridor`),
    /// the way NeoForge's registry `STRUCTURE_POOL_ELEMENT` registers once at
    /// startup: a template pool entry with `"element_type": "modid:corridor"`
    /// then decodes, lays out and places by `ty`'s code, in any structure
    /// that draws from that pool. The id's namespace must be owned by the
    /// calling mod's manifest; another mod's namespace, an empty name and an
    /// id registered twice refuse the mods at startup, naming the mod.
    pub fn register_pool_element_type<T: crate::PoolElementType>(&mut self, name: &str, ty: T) {
        self.add_pool_element_type(name, Arc::new(ty));
    }
}

/// Registrar operations for structures.
pub trait StructuresRegistrar {
    /// Register structure type `name` with `ty`; mods reach it through
    /// `register_structure_type`.
    fn add_structure_type(&mut self, name: &str, ty: Arc<dyn crate::StructureType>);
    /// Register pool element type `name` with `ty`; mods reach it through
    /// `register_pool_element_type`.
    fn add_pool_element_type(&mut self, name: &str, ty: Arc<dyn crate::PoolElementType>);
}
