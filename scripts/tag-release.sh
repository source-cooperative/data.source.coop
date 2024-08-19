if [[ $(git status -s) ]]; then
  echo "ERROR: Please commit all of your changes before tagging the release."
  exit 1
fi

echo "What type of bump would you like to do?"
echo "1) Patch"
echo "2) Minor"
echo "3) Major"

read BUMP_TYPE

if [ $BUMP_TYPE -eq 1 ]; then
  cargo bump patch
elif [ $BUMP_TYPE -eq 2 ]; then
  cargo bump minor
elif [ $BUMP_TYPE -eq 3 ]; then
  cargo bump major
else
  echo "ERROR: Invalid bump type"
  exit 1
fi

VERSION=$(cargo metadata --format-version=1 --no-deps | jq -r '.packages[0].version')
git add Cargo.toml
git add Cargo.lock
git commit -m "Bump version to v$VERSION"
git tag -a "v$VERSION" -m "v$VERSION"
