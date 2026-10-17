import * as fs from 'node:fs/promises';
import { join } from 'node:path';
import { createHash, randomBytes } from 'node:crypto';
import { PNG } from 'pngjs';
import { ProfileError } from './accounts.mjs';
import { atomicJson } from './mod-store.mjs';

export const COPIED_SKIN = /^mc_(wide|slim)_([a-f0-9]{64})$/;
const error = (status, message) => { throw new ProfileError(status, message); };

// Decode and re-encode only bounded Minecraft PNGs, dropping all extra chunks.
// Legacy limbs and alpha follow SkinTextureDownloader.processLegacySkin.
export function normalizeSkin(bytes) {
  if (bytes.length < 33 || bytes.length > 64 * 1024 || !bytes.subarray(0,8).equals(Buffer.from('89504e470d0a1a0a','hex')) || bytes.toString('ascii',12,16) !== 'IHDR' || bytes.readUInt32BE(16) !== 64 || ![32,64].includes(bytes.readUInt32BE(20))) error(422, 'That player does not have a supported Minecraft skin');
  for(let offset=8;offset<bytes.length;) {
    if(offset+12>bytes.length) error(422,'Minecraft returned an invalid skin image');
    const length=bytes.readUInt32BE(offset), type=bytes.toString('ascii',offset+4,offset+8);
    if(offset+12+length>bytes.length || (type==='IHDR' && offset!==8)) error(422,'Minecraft returned an invalid skin image');
    offset+=length+12;
  }
  let decoded;
  try { decoded = PNG.sync.read(bytes, {checkCRC:true}); } catch { error(422, 'Minecraft returned an invalid skin image'); }
  const rgba = Buffer.alloc(64*64*4); decoded.data.copy(rgba);
  const at = (x,y) => (y*64+x)*4;
  if (decoded.height === 32) {
    for (const [sx,sy,ox,oy,w,h] of [[4,16,16,32,4,4],[8,16,16,32,4,4],[0,20,24,32,4,12],[4,20,16,32,4,12],[8,20,8,32,4,12],[12,20,16,32,4,12],[44,16,-8,32,4,4],[48,16,-8,32,4,4],[40,20,0,32,4,12],[44,20,-8,32,4,12],[48,20,-16,32,4,12],[52,20,-8,32,4,12]]) {
      for (let y=0;y<h;y++) for(let x=0;x<w;x++) rgba.copy(rgba,at(sx+ox+w-1-x,sy+oy+y),at(sx+x,sy+y),at(sx+x,sy+y)+4);
    }
    let opaque = true;
    for(let x=32;x<64;x++) for(let y=0;y<32;y++) if(rgba[at(x,y)+3]<128) opaque=false;
    if(opaque) for(let x=32;x<64;x++) for(let y=0;y<32;y++) rgba[at(x,y)+3]=0;
  }
  for(const [x0,y0,x1,y1] of [[0,0,32,16],[0,16,64,32],[16,48,48,64]]) for(let x=x0;x<x1;x++) for(let y=y0;y<y1;y++) rgba[at(x,y)+3]=255;
  return PNG.sync.write({width:64,height:64,data:rgba},{colorType:6,bitDepth:8});
}

export class MinecraftSkins {
  constructor(root, fetcher = fetch) { this.root=join(root,'player-skins'); this.fetch=fetcher; this.active=0; this.cache=new Map(); this.saved=new Map(); this.pending=new Map(); this.retry=new Map(); }
  async init() {
    await fs.mkdir(this.root,{recursive:true,mode:0o700});
    await fs.mkdir(join(this.root,'links'),{recursive:true,mode:0o700});
    for(const file of await fs.readdir(this.root)) if(file.endsWith('.json') && COPIED_SKIN.test(file.slice(0,-5))) {
      const skin=JSON.parse(await fs.readFile(join(this.root,file),'utf8'));
      if(skin.id===file.slice(0,-5) && /^[A-Za-z0-9_]{1,16}$/.test(skin.name)) this.saved.set(skin.id,skin);
    }
    for(const file of await fs.readdir(join(this.root,'links'))) if(/^[a-z0-9_]{1,16}\.json$/.test(file)) {
      const entry=JSON.parse(await fs.readFile(join(this.root,'links',file),'utf8'));
      if(this.describe(entry.skin?.textureId)) this.cache.set(file.slice(0,-5),entry);
    }
  }
  describe(id) { return typeof id==='string' && COPIED_SKIN.test(id) ? this.saved.get(id) : undefined; }
  linked(id) { const m=/^minecraft:([A-Za-z0-9_]{1,16})$/.exec(id??''); return m ? this.cache.get(m[1].toLowerCase())?.skin : undefined; }
  latest(name, fallback, refresh=true) {
    if(!name) return fallback;
    const key=name.toLowerCase(), entry=this.cache.get(key);
    if(refresh && (!entry || entry.expires<=Date.now()) && (this.retry.get(key)??0)<=Date.now() && this.active<2) {
      this.copy(name).catch(()=>{this.retry.set(key,Date.now()+60_000);});
    }
    return entry?.skin ?? fallback;
  }
  async read(url, limit, json = true) {
    let response;
    try { response=await this.fetch(url,{redirect:'error',signal:AbortSignal.timeout(10_000)}); }
    catch { error(502,'Minecraft skin lookup is unavailable. Try again shortly'); }
    if([204,404].includes(response.status)) error(404,'No Minecraft Java player found with that name');
    if(response.status===429) error(429,'Minecraft is busy. Try that name again in a minute');
    if(!response.ok) error(502,'Minecraft skin lookup is unavailable. Try again shortly');
    const chunks=[]; let size=0;
    try {
      for await(const chunk of response.body) { size+=chunk.length; if(size>limit) error(422,'Minecraft returned an oversized skin response'); chunks.push(chunk); }
      const bytes=Buffer.concat(chunks); return json ? JSON.parse(bytes.toString('utf8')) : bytes;
    } catch(e) { if(e instanceof ProfileError) throw e; error(502,'Minecraft returned an unreadable skin response'); }
  }
  async copy(name) {
    if(typeof name!=='string' || !/^[A-Za-z0-9_]{1,16}$/.test(name)) error(400,'Enter a Minecraft Java username: 1–16 letters, numbers, or underscores');
    const key=name.toLowerCase(), cached=this.cache.get(key);
    if(cached?.expires>Date.now()) return cached.skin;
    if(this.pending.has(key)) return this.pending.get(key);
    if(this.active>=2) error(429,'Another skin is being copied. Try again shortly');
    if(!this.cache.has(key) && this.cache.size>=1024) error(503,'The skin library is full. Ask the host for help');
    this.active++;
    const task=(async()=>{ try {
      const profile=await this.read(`https://api.mojang.com/users/profiles/minecraft/${encodeURIComponent(name)}`,16*1024);
      if(!/^[a-f0-9]{32}$/i.test(profile.id??'') || !/^[A-Za-z0-9_]{1,16}$/.test(profile.name??'') || profile.name.toLowerCase()!==key) error(502,'Minecraft returned an unexpected player profile');
      const session=await this.read(`https://sessionserver.mojang.com/session/minecraft/profile/${profile.id}`,32*1024);
      if(session.id!==profile.id) error(502,'Minecraft returned an unexpected skin profile');
      const property=session.properties?.find(p=>p.name==='textures'); let texture;
      try {
        const payload=JSON.parse(Buffer.from(property.value,'base64').toString('utf8'));
        if(payload.profileId!==profile.id) throw new Error('Profile mismatch');
        texture=payload.textures?.SKIN;
      } catch { error(422,'That player does not have a public Minecraft skin'); }
      if(!texture?.url) error(422,'That player has no custom Minecraft skin. Pick a wardrobe skin instead');
      // Never fetch a supplied URL: reconstruct an HTTPS URL on the one allowed host.
      const match=/^https?:\/\/textures\.minecraft\.net\/texture\/([a-f0-9]{32,64})$/.exec(texture.url);
      if(!match) error(422,'Minecraft returned an unsupported skin address');
      const model=texture.metadata?.model==='slim'?'slim':'wide';
      const png=normalizeSkin(await this.read(`https://textures.minecraft.net/texture/${match[1]}`,64*1024,false));
      const hash=createHash('sha256').update(png).digest('hex'), id=`mc_${model}_${hash}`;
      const skin={id,name:profile.name,model,url:`/player-skins/${id}.png`};
      if(!this.saved.has(id)) {
        if(this.saved.size>=4096) error(503,'The skin library is full. Ask the host for help');
        const temporary=join(this.root,`${id}.${randomBytes(12).toString('hex')}.tmp`);
        const file=await fs.open(temporary,'wx',0o600);
        try { await file.writeFile(png); await file.sync(); } finally { await file.close(); }
        await fs.rename(temporary,join(this.root,id+'.png'));
        await atomicJson(join(this.root,id+'.json'),skin);
        this.saved.set(id,skin);
      }
      const linked={...skin,id:`minecraft:${profile.name}`,textureId:id};
      const entry={skin:linked,expires:Date.now()+10*60*1000};
      await atomicJson(join(this.root,'links',key+'.json'),entry);
      this.cache.set(key,entry); this.retry.delete(key);
      return linked;
    } finally { this.active--; this.pending.delete(key); } })();
    this.pending.set(key,task); return task;
  }
}
