package scheduler

import (
	api "github.com/ag-computational-bio/bakta-web-api-go/bakta/web/api/proto/v1"
	"k8s.io/client-go/kubernetes/fake"
	"testing"
)

func TestSimpleScheduler_StartJob(t *testing.T) {
	simpleScheduler, err := InitSimpleScheduler(nil, fake.NewSimpleClientset())
	if err != nil || simpleScheduler == nil {
		t.Fatal("error in simple scheduler init")
	}

	job, err := simpleScheduler.StartJob("test", &api.JobConfig{
		HasProdigal:        false,
		HasReplicons:       false,
		TranslationalTable: 0,
		CompleteGenome:     false,
		KeepContigHeaders:  false,
		MinContigLength:    0,
		DermType:           0,
		Genus:              "",
		Species:            "",
		Strain:             "",
		Plasmid:            "",
		Locus:              "",
		LocusTag:           "",
	}, createBaktaConf())
	if err != nil {
		return
	}

}

func TestSimpleScheduler_GetNamespace(t *testing.T) {

}

func TestSimpleScheduler_DeleteJob(t *testing.T) {

}

func TestSimpleScheduler_GetK8sClient(t *testing.T) {

}
