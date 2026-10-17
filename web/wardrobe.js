const images = new Map();
export async function skinImage(skin) {
  if (!images.has(skin.id)) images.set(skin.id, new Promise((resolve, reject) => {
    const image = new Image(); image.onload = () => resolve(image); image.onerror = () => reject(new Error('The wardrobe could not load. Please refresh'));
    image.src = skin.url;
  }));
  return images.get(skin.id);
}
export function drawCharacter(canvas, image, faceOnly = false) {
  const ctx = canvas.getContext('2d'); ctx.imageSmoothingEnabled = false;
  ctx.clearRect(0, 0, canvas.width, canvas.height);
  if (faceOnly) {
    for (const x of [8, 40]) ctx.drawImage(image, x, 8, 8, 8, 0, 0, canvas.width, canvas.height);
    return;
  }
  const scale = Math.min(canvas.width / 24, canvas.height / 36), ox = (canvas.width - 16 * scale) / 2, oy = (canvas.height - 32 * scale) / 2;
  const part = (sx, sy, w, h, x, y) => ctx.drawImage(image, sx, sy, w, h, ox + x * scale, oy + y * scale, w * scale, h * scale);
  // Front faces and their outer layers in the standard 64×64 wide skin layout.
  for (const [sx, sy, w, h, x, y] of [[8,8,8,8,4,0],[20,20,8,12,4,8],[44,20,4,12,0,8],[36,52,4,12,12,8],[4,20,4,12,4,20],[20,52,4,12,8,20],
    [40,8,8,8,4,0],[20,36,8,12,4,8],[44,36,4,12,0,8],[52,52,4,12,12,8],[4,36,4,12,4,20],[4,52,4,12,8,20]]) part(sx,sy,w,h,x,y);
}
