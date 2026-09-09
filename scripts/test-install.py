import os, pathlib, tempfile, subprocess, tarfile, hashlib
root=pathlib.Path(__file__).resolve().parent.parent
for repo,name,binaries in [('mcp-editor-lsp-bridge', 'editor-bridge', ['bridge', 'mcp-editor-lsp-bridge'])]:
 with tempfile.TemporaryDirectory() as tmp:
  p=pathlib.Path(tmp); mock=p/'mock';mock.mkdir(); fixture=p/f'{name}-darwin-arm64';fixture.mkdir()
  for binary in binaries:
   f=fixture/binary;f.write_text('#!/bin/sh\necho fixture-version\n');f.chmod(0o755)
  archive=p/f'{fixture.name}.tar.gz'
  with tarfile.open(archive,'w:gz') as t:t.add(fixture,arcname=fixture.name)
  checksum=p/'checksum';checksum.write_text(hashlib.sha256(archive.read_bytes()).hexdigest()+'  '+archive.name+'\n')
  curl=mock/'curl';curl.write_text('''#!/bin/sh
output=''
url=''
while [ "$#" -gt 0 ]; do
 case "$1" in
  -o) output=$2; shift 2 ;;
  https://*) url=$1; shift ;;
  *) shift ;;
 esac
done
case "$url" in
 *.sha256) cp "$FIXTURE/checksum" "$output" ;;
 *) cp "$FIXTURE/ARCHIVE" "$output" ;;
esac
'''.replace('ARCHIVE',archive.name));curl.chmod(0o755)
  uname=mock/'uname';uname.write_text('#!/bin/sh\ncase "$1" in -s) echo Darwin;; -m) echo arm64;; esac\n');uname.chmod(0o755)
  env=dict(os.environ,PATH=str(mock)+':'+os.environ['PATH'],FIXTURE=str(p),INSTALL_VERSION='v0.0.1',INSTALL_ROOT=str(p/'installed'),INSTALL_BIN_DIR=str(p/'bin'))
  bundled = p/'bundled-install.sh'
  bundled.write_bytes(subprocess.check_output(['sh', str(root/'scripts/bundle-installer.sh')]))
  subprocess.run(['sh', '-n', str(bundled)], check=True)
  def run(ok, source=False):
   r=subprocess.run(['sh',str(root/'scripts/install.sh')] if source else ['sh'], input=None if source else bundled.read_text(), cwd=p, env=env,capture_output=True,text=True)
   assert (r.returncode==0)==ok,r.stdout+r.stderr
   return r
  run(True, source=True); first=(p/'installed/current').resolve();run(True)
  assert first != (p/'installed/current').resolve()
  assert first.exists()
  assert subprocess.check_output([str(p/'bin'/binaries[0])],text=True).strip()=='fixture-version'
  before=(p/'installed/current').resolve();checksum.write_text('0'*64+'  '+archive.name+'\n')
  assert 'Checksum mismatch' in run(False).stderr
  assert (p/'installed/current').resolve()==before
  checksum.write_text(hashlib.sha256(archive.read_bytes()).hexdigest()+'  '+archive.name+'\n')
  launcher=p/'bin'/binaries[0];launcher.unlink();launcher.write_text('existing executable')
  assert 'already exists' in run(False).stderr
  assert launcher.read_text()=='existing executable'
  print(repo+': fresh install, upgrade, executable link, bad checksum, existing-file preservation passed')
