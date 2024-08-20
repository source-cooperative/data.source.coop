VERSION=$(git tag --points-at HEAD)

# Check if the current commit is a release commit
if [ -z "$VERSION" ]; then
    echo "No release tag found for this commit. Are you sure you checked out a release commit?"
    exit 1;
fi

# Check if the image for the current version exists in ECR
if [ -z "$(aws ecr describe-images --repository-name source-data-proxy --image-ids=imageTag=$VERSION --profile opendata 2> /dev/null)" ]; then
  echo "Could not find image for version $VERSION in ECR. Did you build and push the image?"
  exit 1;
fi

echo "Deploying $VERSION..."

contents="$(jq ".containerDefinitions[0].image = \"417712557820.dkr.ecr.us-west-2.amazonaws.com/source-data-proxy:$VERSION\"" scripts/task_definition.json)" && \
echo "${contents}" > scripts/task_definition_deploy.json

# Register the task definition
if [ -z "$(aws ecs register-task-definition --cli-input-json "file://scripts/task_definition_deploy.json" --profile opendata --no-cli-auto-prompt 2> /dev/null)" ]; then
  echo "Failed to create task definition"
  echo "Cleaning Up..."
  rm scripts/task_definition_deploy.json
  exit 1;
fi

echo "Created Task Definition"

TASK_DEFINITION_ARN=$(aws ecs list-task-definitions --family-prefix source-data-proxy --status ACTIVE --profile opendata --query "taskDefinitionArns[-1]" --output text)

echo "Updating Service..."

if [ -z "$(aws ecs update-service --cluster SourceCooperative-Prod --service source-data-proxy --task-definition $TASK_DEFINITION_ARN --profile opendata 2> /dev/null)" ]; then
  echo "Failed to update service"
  echo "Cleaning Up..."
  rm scripts/task_definition_deploy.json
  exit 1;
fi

echo "Cleaning Up..."
rm scripts/task_definition_deploy.json
