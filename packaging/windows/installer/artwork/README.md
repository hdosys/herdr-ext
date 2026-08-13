# Installer artwork

`installer-welcome-finish-source.png` is the single high-resolution source for
the NSIS Welcome and Finish artwork. The source is not embedded in setup; only
the five native-size BMP3 derivatives are packaged.

The checked-in derivatives were rendered on Windows with ImageMagick
7.1.2-29 Q16-HDRI. Each target is rendered directly from the source in linear
RGB with Lanczos3, then converted back to 8-bit sRGB:

```powershell
$magick = "C:\Program Files\ImageMagick-7.1.2-Q16-HDRI\magick.exe"
$source = "installer-welcome-finish-source.png"
foreach ($size in @("164x314", "205x393", "246x471", "287x550", "328x628")) {
    & $magick $source `
        -colorspace RGB `
        -filter Lanczos `
        -define filter:lobes=3 `
        -resize "$size!" `
        -colorspace sRGB `
        -alpha off `
        -depth 8 `
        -type TrueColor `
        -strip `
        -compress None `
        "BMP3:installer-welcome-finish-$size.bmp"
}
```
