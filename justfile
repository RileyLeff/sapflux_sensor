# Add a component from LCSC to the local library
add part_number:
    uvx JLC2KiCadLib {{part_number}} \
      -dir common_lib \
      -symbol_lib_dir symbol \
      -symbol_lib sap_symbols \
      -footprint_lib sap_footprints \
      -model_dir ../3d_models \
      -model_base_variable '${KIPRJMOD}/common_lib/3d_models'
