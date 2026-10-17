const images = new Map();
export async function skinImage(skin) {
  if (!images.has(skin.url)) images.set(skin.url, new Promise((resolve, reject) => {
    const image = new Image(); image.onload = () => resolve(image); image.onerror = () => reject(new Error('The wardrobe could not load. Please refresh'));
    image.src = skin.url;
  }));
  return images.get(skin.url);
}
export function drawCharacter(canvas, image, faceOnly = false, model = 'wide') {
  const ctx = canvas.getContext('2d'); ctx.imageSmoothingEnabled = false;
  ctx.clearRect(0, 0, canvas.width, canvas.height);
  if (faceOnly) {
    for (const x of [8, 40]) ctx.drawImage(image, x, 8, 8, 8, 0, 0, canvas.width, canvas.height);
    return;
  }
  const scale = Math.min(canvas.width / 24, canvas.height / 36), ox = (canvas.width - 16 * scale) / 2, oy = (canvas.height - 32 * scale) / 2;
  const part = (sx, sy, w, h, x, y) => ctx.drawImage(image, sx, sy, w, h, ox + x * scale, oy + y * scale, w * scale, h * scale);
  const arm = model === 'slim' ? 3 : 4;
  // Both Minecraft arm models use the same front-face origins.
  for (const [sx, sy, w, h, x, y] of [[8,8,8,8,4,0],[20,20,8,12,4,8],[44,20,arm,12,4-arm,8],[36,52,arm,12,12,8],[4,20,4,12,4,20],[20,52,4,12,8,20],
    [40,8,8,8,4,0],[20,36,8,12,4,8],[44,36,arm,12,4-arm,8],[52,52,arm,12,12,8],[4,36,4,12,4,20],[4,52,4,12,8,20]]) part(sx,sy,w,h,x,y);
}
