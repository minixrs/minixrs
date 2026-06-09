# external/limine/Makefile
#
# Downloads the Limine bootloader binary distribution at a pinned commit on
# the v9.x-binary branch and extracts the artifacts minix.rs needs:
#   - BOOTAA64.EFI   aarch64 UEFI bootloader binary
#   - limine.h       protocol header (reference; Rust bindings live in kernel/)
#
# Run `make` once after cloning. Outputs land in $(DIST)/ and are gitignored.

LIMINE_COMMIT  = ee5d29cd0a8034612dcd1df3f00052480db785c5
LIMINE_URL     = https://github.com/limine-bootloader/limine/archive/$(LIMINE_COMMIT).tar.gz
# GitHub archives top-level dir is "Limine-<sha>" (capital L) for this repo.
LIMINE_SRCDIR  = Limine-$(LIMINE_COMMIT)
LIMINE_TARBALL = dist/$(LIMINE_SRCDIR).tar.gz

DIST = dist

.PHONY: all clean distclean

all: $(DIST)/BOOTAA64.EFI $(DIST)/limine.h

$(LIMINE_TARBALL):
	@mkdir -p $(DIST)
	@echo "==> Downloading Limine @ $(LIMINE_COMMIT)..."
	curl -fL -o $@ $(LIMINE_URL)

$(DIST)/BOOTAA64.EFI $(DIST)/limine.h: $(LIMINE_TARBALL)
	@echo "==> Extracting Limine artifacts..."
	tar xzf $(LIMINE_TARBALL) --strip-components=1 -C $(DIST) \
		$(LIMINE_SRCDIR)/BOOTAA64.EFI \
		$(LIMINE_SRCDIR)/limine.h
	@touch $(DIST)/BOOTAA64.EFI $(DIST)/limine.h

clean:
	rm -f $(DIST)/BOOTAA64.EFI $(DIST)/limine.h

distclean:
	rm -rf $(DIST)
