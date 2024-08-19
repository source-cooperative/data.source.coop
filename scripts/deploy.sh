TAG=$(git tag --points-at HEAD)

if [ -z "$TAG" ]; then
    echo "No release tag found for this commit. Are you sure you checked out a release commit?"
    exit 1;
fi

echo "Deploying $TAG..."
