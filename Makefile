SCALPEL_MANIFEST := tools/scalpel-cli/Cargo.toml
SCALPEL_BIN := tools/scalpel-cli/target/release/scalpel-cli
SKILL_BIN := skills/Scalpel/scripts/scalpel

.PHONY: build check clean

build:
	cargo build --release --manifest-path $(SCALPEL_MANIFEST)
	cp $(SCALPEL_BIN) $(SKILL_BIN)

check:
	cargo check --manifest-path $(SCALPEL_MANIFEST)

clean:
	rm -f $(SKILL_BIN)
