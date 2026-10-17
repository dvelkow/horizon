# (dev)Modcraft artwork

The site uses `pepe-pixel-party-v1.png`, generated with the built-in image generation
tool. The title and controls are real HTML, kept separate from the illustration.
Geist fonts are bundled locally under the accompanying SIL Open Font License.

Selected artwork: **Pixel Pepe Party** (option 5).

Design: a dense, irregular rectangular pixel-art crowd surrounds one quiet central menu.
Ice blue `#dcebef`, frog green `#4c8c3f`, dark outline `#203426`, pale highlight
`#f5ffe9`, and warm yellow `#f0c861` complement the illustration. Geist Semibold
carries the large centered wordmark; Geist Regular keeps forms readable. Chunky
beveled buttons echo the game's controls. No floating cards or extra decoration
compete with the crowd. On narrow screens, the side crowds crop to preserve a
usable central form, with the top and bottom Pepes still visible.

The prompt below was used verbatim in built-in mode (no API/CLI fallback),
with the original `pepe-crowd-v1.png` as the composition reference. The original
artwork is preserved in Git history before this selection.

```text
Use case: illustration-story.
Asset type: wide 16:9 illustrated background for the (dev)Modcraft game main menu, image only.
Input image 1 is a composition and character reference for a new alternate design, not an image to copy exactly.
Composition: a playful irregular frame along all four outside edges around a completely clean empty softly rounded RECTANGLE in the center. Preserve generous blank space from approximately x=28% to 72%, y=16% to 84%, with subtly rounded corners and organically uneven boundary. This is a tall usable central rectangle within a landscape canvas, NOT a circular opening. Keep every character, prop and mark outside the central blank area. The edge characters can be cropped by the outside canvas edges. Match the reference's usable central opening.
Backdrop: very pale ice blue #dcebef, completely unmarked center.
No lettering, words, logos, buttons, interface elements, watermarks, political symbols. This is just the background illustration; website text will be placed separately.
Primary request: reinterpret the reference as a polished detailed PIXEL ART game-menu border. Around 24 different pixelated Pepe the Frog characters in a joyous chaotic party along all four edges: baby Pepe, wizard Pepe, genius brain Pepe, coder with chunky laptop, grass-block builder, coffee Pepe, surprised Pepe, laughing Pepe, astronaut Pepe, artist Pepe, gamer headphones Pepe. Truly crisp visible square pixels with controlled stepped contours, limited coherent game palette, deliberate pixel clusters and chunky retro shading, no smooth vector outlines or blur. Strong distinct silhouettes and recognizable Pepe heavy eyelids and orange lips despite pixel treatment. Include a few voxel grass cubes among the outer-edge crowd, not a full landscape. Preserve the broad clean smooth-cornered rectangular central opening using a delicately stepped pixel edge. Central background is plain pale ice blue, no pixels or decoration in its interior.
```
