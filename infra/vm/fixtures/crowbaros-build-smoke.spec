Name:           crowbaros-build-smoke
Version:        0.1.0
Release:        1%{?dist}
Summary:        Development-only CrowbarOS RPM pipeline probe
License:        MIT
Source0:        crowbaros-build-smoke.rs
BuildRequires:  rust

%description
A tiny Rust executable used only to validate the local Fedora packaging lab.
This is not a CrowbarOS application or a release package.

%prep

%build
rustc --crate-name crowbaros_build_smoke --edition 2021 -C opt-level=2 %{SOURCE0} -o crowbaros-build-smoke

%install
install -Dpm0755 crowbaros-build-smoke %{buildroot}%{_bindir}/crowbaros-build-smoke

%files
%{_bindir}/crowbaros-build-smoke
