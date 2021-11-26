package argoclient

import (
	"context"
	"fmt"
	"github.com/argoproj/argo-workflows/v3/cmd/argo/commands/client"
	"github.com/argoproj/argo-workflows/v3/pkg/apiclient"
	workflowpkg "github.com/argoproj/argo-workflows/v3/pkg/apiclient/workflow"
	wfv1 "github.com/argoproj/argo-workflows/v3/pkg/apis/workflow/v1alpha1"
	metav1 "k8s.io/apimachinery/pkg/apis/meta/v1"
	"time"
)

type ArgoClient struct {
	ctx                   context.Context
	client                apiclient.Client
	wfService             workflowpkg.WorkflowServiceClient
	namespace, wfTemplate string
}

var (
	defaultFields = "items.metadata.labels,items.status.phase,items.status.message,items.status.finishedAt,items.status.startedAt,items.status.estimatedDuration,items.status.progress"
)

func NewClient(namespace, workflowTemplate string) *ArgoClient {
	ctx, apiClient := client.NewAPIClient(context.Background())
	serviceClient := apiClient.NewWorkflowServiceClient()

	return &ArgoClient{
		ctx:        ctx,
		client:     apiClient,
		wfService:  serviceClient,
		namespace:  namespace,
		wfTemplate: workflowTemplate,
	}
}

func (argo *ArgoClient) SubmitBaktaWorkflow(name, jobid, secret, confstring string) error {

	submitOpts := argo.CreateSubmitOpts(name, jobid, secret, confstring)

	_, err := argo.wfService.SubmitWorkflow(argo.ctx, &workflowpkg.WorkflowSubmitRequest{
		Namespace:     argo.namespace,
		ResourceKind:  "workflowtemplate",
		ResourceName:  argo.namespace,
		SubmitOptions: submitOpts,
	})
	if err != nil {
		return err
	}

	return nil
}

func (argo *ArgoClient) GetWorkflowStatus() (wfs *map[string]WorkflowStatus, err error) {

	listOpts := &metav1.ListOptions{}

	wfList, err := argo.wfService.ListWorkflows(argo.ctx, &workflowpkg.WorkflowListRequest{
		Namespace:   argo.namespace,
		ListOptions: listOpts,
		Fields:      defaultFields,
	})

	wfmap := map[string]WorkflowStatus{}

	if err != nil {
		return nil, err
	}
	for _, x := range wfList.Items {

		updateTime := time.Now()
		if x.Status.Phase == "Succeeded" {
			updateTime = x.Status.FinishedAt.Time
		}

		wfmap[x.Labels["jobid"]] = WorkflowStatus{
			JobId:   x.Labels["jobid"],
			Name:    x.Labels["jobid"],
			Secret:  x.Labels["jobid"],
			Status:  string(x.Status.Phase),
			Message: x.Status.Message,
			Started: x.Status.StartedAt.Time,
			Updated: updateTime,
		}
	}

	return &wfmap, nil

}

func (argo *ArgoClient) CreateSubmitOpts(name, jobid, secret, confstring string) *wfv1.SubmitOpts {

	return &wfv1.SubmitOpts{
		GenerateName: fmt.Sprintf("bakta-%v-", jobid),
		Parameters:   []string{fmt.Sprintf("parameter=%s", confstring)},
		Labels:       fmt.Sprintf("name=%v,jobid=%v,secret=%v", name, jobid, secret),
	}
}
