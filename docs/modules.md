# Modules

| module | format |
| --- | --- |
| `toc`, `bundle`, `db`, `layout` | superbundle TOCs (Huffman-coded names in the base game, plain names in Patch TOCs, the perfect-hash tables, the Patch merge), a bundle's file table, binary manifest and chunk metadata, the native DB format, and `layout.toc` typed |
| `cas`, `folder`, `listing`, `scope`, `names` | CAS archives (raw, zlib, LZ4 and Oodle blocks), a data folder with its archives, the bundles and entries a TOC lists, ids and names looked up across several bundles, and a table from EBX file GUIDs to document names |
| `ebx` | EBX documents (RIFF `EBX`/`EBXS`, REFL or RFL2 reflection) read into typed values, written again, carried between documents (`ebx::carry`) and given new GUIDs (`ebx::remap`) |
| `fbmod` | `.fbmod` mods in the layout ReSkate Studio writes (Frosty mod version 6), payloads included |
| `depot`, `paramdb`, `lookup`, `selector`, `permutation`, `binding`, `inputlayout`, `bytecode` | the shader database: block depots, the keys of their parameters and the names behind them, program and texture lookup tables, the permutation selector, permutations with their fixed-function state, binding sets, input layouts, and the DXBC/DXIL containers of compiled bytecode |
| `meshset`, `rigid`, `tangent`, `physics`, `skeleton`, `texture` | MeshSets (LODs, sections, vertex declarations), rigid meshes built from triangles, the packed tangent frame, physics resources (with an encoder for triangle meshes), skeleton assets, 180-byte texture headers |

The formats follow the skate. build installed from the game (Skate.exe build 20260929) and the files ReSkate Studio 20261012 writes. Other Frostbite titles are not assumed to match.
