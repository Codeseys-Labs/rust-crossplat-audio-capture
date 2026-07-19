// rsac-f18f — Objective-C bridging header for the iOS SIMULATOR TCC harness
// test bundle. Exposes the curated rsac C API (rsac-ffi) to Swift.
//
// HEADER_SEARCH_PATHS (project.yml) points at bindings/rsac-ffi/include, so
// `rsac.h` is the maintained, hand-curated surface — NOT rsac_generated.h.
// The `compose` feature is off in the CI build, so RSAC_FEATURE_COMPOSE stays
// undefined and the compose declarations are preprocessed away (matching the
// symbols actually present in librsac_ffi.a).
#import "rsac.h"
