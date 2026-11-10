// An image onto the macOS pasteboard for `tenx web` (clipboard.rs), run by
// `osascript -l JavaScript` with the pasteboard's name ('' for the general
// one) and the image's pasteboard type; the image's bytes come on stdin.
ObjC.import('AppKit');

function run(argv) {
  const pb = argv[0] ? $.NSPasteboard.pasteboardWithName(argv[0]) : $.NSPasteboard.generalPasteboard;
  const data = $.NSFileHandle.fileHandleWithStandardInput.readDataToEndOfFile;
  pb.clearContents;
  if (!pb.setDataForType(data, argv[1])) throw new Error('the pasteboard refused the image');
  // Not every app pastes JPEG, GIF or WebP; all of them paste TIFF.
  if (argv[1] !== 'public.png') {
    const rep = $.NSBitmapImageRep.imageRepWithData(data);
    if (!rep.isNil()) pb.setDataForType(rep.TIFFRepresentation, 'public.tiff');
  }
}
