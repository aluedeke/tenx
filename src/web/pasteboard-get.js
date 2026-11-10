// The macOS pasteboard for `tenx web` (clipboard.rs), run by
// `osascript -l JavaScript` with: the pasteboard's name ('' for the general
// one), the change count the caller already knows ('' for none), and the
// largest image to hand over, in bytes.
//
// Prints one JSON line — `{count}` when the count hasn't moved, else
// `{count, text, file, image?: {type, bytes}}` (`file`: copied in Finder) —
// and then the image's bytes.
ObjC.import('AppKit');

const TYPES = { 'public.png': 'image/png', 'public.jpeg': 'image/jpeg', 'com.compuserve.gif': 'image/gif', 'org.webmproject.webp': 'image/webp' };
const FILES = { png: 'image/png', jpg: 'image/jpeg', jpeg: 'image/jpeg', gif: 'image/gif', webp: 'image/webp' };
// Read by AppKit and handed over as PNG.
const CONVERTED = ['heic', 'heif', 'tif', 'tiff', 'bmp'];

function png(data) {
  const rep = $.NSBitmapImageRep.imageRepWithData(data);
  return rep.isNil() ? null : rep.representationUsingTypeProperties($.NSBitmapImageFileTypePNG, $());
}

// A file copied in Finder: the image file itself, never its icon.
function copiedFile(pb, max) {
  const items = pb.pasteboardItems;
  if (Number(items.count) !== 1) return null;
  const url = $.NSURL.URLWithString(items.objectAtIndex(0).stringForType('public.file-url'));
  if (url.isNil()) return null;
  const ext = url.pathExtension.js.toLowerCase();
  if (!FILES[ext] && !CONVERTED.includes(ext)) return null;
  const attrs = $.NSFileManager.defaultManager.attributesOfItemAtPathError(url.path, null);
  if (attrs.isNil() || Number(attrs.fileSize) > max) return null;
  const data = $.NSData.dataWithContentsOfURL(url);
  if (data.isNil()) return null;
  return FILES[ext] ? { type: FILES[ext], data } : { type: 'image/png', data: png(data) };
}

function copiedImage(pb, types) {
  const uti = Object.keys(TYPES).find((t) => types.includes(t));
  if (uti) return { type: TYPES[uti], data: pb.dataForType(uti) };
  // Preview and others copy TIFF alone.
  if (types.includes('public.tiff')) return { type: 'image/png', data: png(pb.dataForType('public.tiff')) };
  return null;
}

function run(argv) {
  const pb = argv[0] ? $.NSPasteboard.pasteboardWithName(argv[0]) : $.NSPasteboard.generalPasteboard;
  const max = Number(argv[2]);
  const head = { count: Number(pb.changeCount) };
  let image = null;
  if (String(head.count) !== argv[1]) {
    const text = pb.stringForType('public.utf8-plain-text');
    head.text = text.isNil() ? '' : text.js;
    const types = ObjC.deepUnwrap(pb.types) || [];
    // Finder's text is the file's name, spaces and all: not text of its own.
    head.file = types.includes('public.file-url');
    // Text of several words wins over an image (clipboard.rs decides): the
    // picture a spreadsheet adds to copied cells is not read at all.
    if (head.file || !/\s/.test(head.text.trim())) {
      image = head.file ? copiedFile(pb, max) : copiedImage(pb, types);
    }
    if (image && image.data && !image.data.isNil() && Number(image.data.length) <= max) {
      head.image = { type: image.type, bytes: Number(image.data.length) };
    } else {
      image = null;
    }
  }
  const out = $.NSFileHandle.fileHandleWithStandardOutput;
  out.writeData($(JSON.stringify(head) + '\n').dataUsingEncoding($.NSUTF8StringEncoding));
  if (image) out.writeData(image.data);
}
